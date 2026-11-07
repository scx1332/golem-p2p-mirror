//! Turns the measurements of a bench run into a status, an exit code and
//! plain-language findings.
//!
//! Every statement is based on something that was measured in this run, and
//! is worded no stronger than the measurement allows. What could not be
//! measured is listed in `unknowns`.

use super::report::{
    Finding, Latency, NetReport, Peer, RelayCheck, Report, Status, TransferReport, Unknown,
    fmt_mbps, fmt_ms, short_id,
};

/// Packet loss in the latency phase from which on the `packet_loss` hint is
/// given. QUIC's congestion control (loss based, Cubic) treats every loss as
/// congestion, so random loss caps a connection's throughput at roughly
/// `1/sqrt(loss)` times what one packet per round trip gives (Mathis et al.):
/// at 0.5% and 100 ms that is a few Mbps. The lab's 1% loss link cut 123 Mbps
/// to 2-26 Mbps.
const LOSS_HINT_PCT: f64 = 0.5;

/// Fewer packets sent in the latency phase say too little about loss. With
/// the defaults (10 s, one request per 200 ms) the phase sends about 110.
const LOSS_MIN_PACKETS: u64 = 50;

/// Fills `status`, `exit_code`, `verdict`, `issues`, `hints` and `unknowns`.
pub fn analyze(r: &mut Report) {
    let f = Facts::new(r);
    let mut issues = Vec::new();
    let mut hints = Vec::new();
    let mut unknowns = Vec::new();
    f.local_findings(&mut issues, &mut hints, &mut unknowns);
    f.peer_findings(&mut issues, &mut hints, &mut unknowns);
    let status = match f.peer {
        Some(p) if !p.connect.ok => Status::NoConnection,
        Some(_) if !f.direct_seen => Status::RelayOnly,
        _ if !issues.is_empty() => Status::Degraded,
        _ => Status::Healthy,
    };
    let (mut verdict, in_verdict) = f.headline(status);
    if r.interrupted {
        verdict = format!("Interrupted, the results are incomplete. {verdict}");
    }
    r.status = status;
    r.exit_code = status.exit_code();
    r.verdict = verdict;
    r.issues = issues;
    r.hints = hints;
    r.unknowns = unknowns;
    r.in_verdict = in_verdict.into_iter().collect();
}

fn finding(code: &'static str, message: impl Into<String>) -> Finding {
    Finding {
        code,
        message: message.into(),
    }
}

fn unknown(item: &'static str, reason: impl Into<String>) -> Unknown {
    Unknown {
        item,
        reason: reason.into(),
    }
}

/// What the measurements say, in one place.
struct Facts<'a> {
    r: &'a Report,
    relay_only_mode: bool,
    nr: Option<&'a NetReport>,
    /// At least one relay has QUIC address discovery configured.
    qad_configured: bool,
    reachable_relays: usize,
    /// QAD configured, a relay reachable over HTTPS, a net report, and still
    /// no relay answered over UDP.
    no_udp_answer: bool,
    hard_nat: bool,
    peer: Option<&'a Peer>,
    direct_seen: bool,
}

impl<'a> Facts<'a> {
    fn new(r: &'a Report) -> Self {
        let local = r.local.as_ref();
        let relay_only_mode = r.transport == "relay_only";
        let nr = local.and_then(|l| l.net_report.as_ref());
        let qad_configured = local.is_some_and(|l| l.relays.iter().any(|x| x.quic_port.is_some()));
        let reachable_relays = local.map_or(0, |l| l.relays.iter().filter(|x| x.ok).count());
        let no_udp_answer = !relay_only_mode
            && qad_configured
            && reachable_relays > 0
            && nr.is_some_and(|n| !n.udp_v4 && !n.udp_v6);
        let peer = r.peer.as_ref();
        Self {
            r,
            relay_only_mode,
            nr,
            qad_configured,
            reachable_relays,
            no_udp_answer,
            hard_nat: nr.and_then(|n| n.mapping_varies_by_dest) == Some(true),
            peer,
            direct_seen: peer.is_some_and(|p| p.path.time_to_direct_ms.is_some()),
        }
    }

    fn peer_name(&self) -> String {
        self.r.to.as_deref().map(short_id).unwrap_or_default()
    }

    fn timeout(&self) -> String {
        fmt_ms(self.r.settings.timeout_ms)
    }

    fn local_findings(
        &self,
        issues: &mut Vec<Finding>,
        hints: &mut Vec<Finding>,
        unknowns: &mut Vec<Unknown>,
    ) {
        let Some(l) = &self.r.local else {
            if self.r.settings.skip.contains(&"local") {
                unknowns.push(unknown(
                    "local_network",
                    "the local network check was skipped (--skip local)",
                ));
            }
            return;
        };
        for c in l.relays.iter().filter(|c| !c.ok) {
            issues.push(finding(
                "relay_unreachable",
                format!(
                    "Relay {} is not reachable: {}.{}",
                    c.url,
                    c.error.as_deref().unwrap_or("unknown error"),
                    relay_advice(c)
                ),
            ));
        }
        if let Some(h) = &l.home_relay
            && !h.connected
            && self.reachable_relays > 0
        {
            issues.push(finding(
                "no_home_relay",
                format!(
                    "This node is not connected to any relay{}, so peers that cannot reach it \
                     directly cannot reach it at all.",
                    h.error
                        .as_ref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                ),
            ));
        }
        for d in &l.directories {
            if !d.ok {
                issues.push(finding(
                    "directory_unreachable",
                    format!(
                        "Directory {} is not reachable: {}.",
                        d.url,
                        d.error.as_deref().unwrap_or("unknown error")
                    ),
                ));
            } else if d.own_record == Some("other_endpoint") {
                issues.push(finding(
                    "identity_in_use",
                    format!(
                        "Directory {} lists another endpoint for this node ID: another \
                         golem-p2p process with the same key published its record after this \
                         one, and peers will connect to that process.",
                        d.url
                    ),
                ));
            }
        }
        if l.record_published == Some(false)
            && !l
                .directories
                .iter()
                .any(|d| d.own_record == Some("other_endpoint"))
        {
            issues.push(finding(
                "record_not_published",
                "This node's record is in none of the directories, so peers cannot find it.",
            ));
        }

        if self.no_udp_answer {
            if self.direct_seen {
                hints.push(finding(
                    "qad_failed",
                    "No relay answered QUIC address discovery, but the direct connection to the \
                     peer works, so UDP is not blocked. Check the relays' QAD port \
                     (--relay URL,quic=PORT).",
                ));
            } else {
                let ports: Vec<String> = l
                    .relays
                    .iter()
                    .filter_map(|c| c.quic_port.map(|p| p.to_string()))
                    .collect();
                issues.push(finding(
                    "udp_blocked",
                    format!(
                        "No relay answered over UDP (QUIC address discovery, UDP port {}), \
                         although they answer over HTTPS. Outgoing UDP is probably blocked on \
                         your network, so connections go through a relay. Ask your network \
                         admin to allow outgoing UDP.",
                        ports.join(", ")
                    ),
                ));
            }
        }
        if self.hard_nat {
            hints.push(finding(
                "hard_nat",
                "Your NAT changes the public port for every destination (hard NAT). Direct \
                 connections to peers behind similar NATs will fall back to relays.",
            ));
        }
        if l.relays.is_empty() {
            hints.push(finding(
                "no_relays",
                "No relay is configured: only peers that can reach this node directly can \
                 connect to it, and hole punching is not possible.",
            ));
        }

        unknowns.push(unknown(
            "port_mapping",
            "port mapping (UPnP, PCP, NAT-PMP): golem-p2p deliberately does not use it, so it \
             never changes your router's settings; whether your router supports it is not \
             probed",
        ));
        match self.nr {
            None if l.relays.is_empty() => unknowns.push(unknown(
                "udp",
                "UDP, the public address and the NAT need a relay with QUIC address discovery; \
                 none is configured",
            )),
            None if self.relay_only_mode => unknowns.push(unknown(
                "udp",
                "UDP, the public address and the NAT were not tested (--relay-only)",
            )),
            None => unknowns.push(unknown(
                "udp",
                "iroh's network report did not finish in time; UDP, the public address and the \
                 NAT are unknown",
            )),
            Some(_) if !self.qad_configured => unknowns.push(unknown(
                "udp",
                "QUIC address discovery is off for all relays (quic=off), so UDP, the public \
                 address and the NAT could not be measured",
            )),
            // Without a NAT (public IPv4 address on this machine) there is
            // no mapping to vary.
            Some(n)
                if n.mapping_varies_by_dest.is_none()
                    && (n.udp_v4 || n.udp_v6)
                    && n.behind_nat_v4 != Some(false) =>
            {
                unknowns.push(unknown(
                    "nat_mapping",
                    format!(
                        "whether your NAT keeps the same public port for every destination \
                         needs UDP answers from at least two relays (got {})",
                        n.qad_relays_v4.max(n.qad_relays_v6)
                    ),
                ))
            }
            Some(_) => {}
        }
    }

    fn peer_findings(
        &self,
        issues: &mut Vec<Finding>,
        hints: &mut Vec<Finding>,
        unknowns: &mut Vec<Unknown>,
    ) {
        let Some(p) = self.peer else { return };
        if !p.connect.ok {
            issues.push(finding(
                "connect_failed",
                format!(
                    "Could not connect: {}.",
                    p.connect.error.as_deref().unwrap_or("unknown error")
                ),
            ));
            return;
        }
        // A signal cancels the request in flight; that is not a failure.
        let interrupted = u64::from(self.r.interrupted);
        if let Some(l) = &p.latency
            && l.failed > interrupted
        {
            issues.push(finding(
                "requests_failed",
                format!(
                    "{} of {} requests got no correct reply (timeout {}).",
                    l.failed,
                    l.sent,
                    self.timeout()
                ),
            ));
        }
        for (what, t) in [("Upload", &p.upload), ("Download", &p.download)] {
            if let Some(t) = t
                && !t.ok
            {
                let code = if t.error_kind == Some("refused") {
                    "transfer_refused"
                } else {
                    "transfer_failed"
                };
                issues.push(finding(
                    code,
                    format!(
                        "{what} failed: {}.",
                        t.error.as_deref().unwrap_or("unknown error")
                    ),
                ));
            }
        }
        if let Some(c) = &p.concurrency
            && (!c.ok || c.failed > 0)
        {
            issues.push(finding(
                "parallel_failed",
                format!(
                    "{} of {} parallel requests failed{}.",
                    c.failed,
                    c.requests + c.failed,
                    c.error
                        .as_ref()
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default()
                ),
            ));
        }
        // Losing the connection while idle on purpose is the idle phase's
        // finding (`idle_reconnect`).
        if let Some(c) = &p.connection_lost
            && c.phase != "idle"
        {
            let why = match c.cause {
                "closed_here" => format!(
                    "a request got no answer, and nothing at all arrived from the peer for {} \
                     (it may have crashed or hung, or the network path broke), so this node \
                     closed the connection",
                    self.timeout()
                ),
                "idle_timeout" => "nothing arrived from the peer for QUIC's idle timeout (it may \
                                   have crashed, or the network path broke)"
                    .into(),
                "closed_by_peer" => format!("the peer closed it ({})", c.reason),
                "reset" => "the peer no longer knew the connection (it probably restarted)".into(),
                _ => c.reason.clone(),
            };
            let after = if p.path.final_path == "unknown" {
                "; at the end there was no connection"
            } else {
                "; a later request connected again"
            };
            issues.push(finding(
                "connection_lost",
                format!(
                    "The connection was lost after {}, during the {} phase: {why}{after}.",
                    fmt_ms(c.t_ms),
                    c.phase.replace('_', " "),
                ),
            ));
        }
        if self.direct_seen && p.path.final_path == "relay" {
            issues.push(finding(
                "direct_path_lost",
                "A direct path was found, but at the end the connection went through the relay \
                 again.",
            ));
        }
        if let Some(i) = &p.idle {
            if !i.ok {
                issues.push(finding(
                    "idle_failed",
                    format!(
                        "After {} without traffic the peer could not be reached again: {}.",
                        fmt_ms(i.idle_ms),
                        i.error.as_deref().unwrap_or("unknown error")
                    ),
                ));
            } else if !i.survived {
                hints.push(finding(
                    "idle_reconnect",
                    format!(
                        "The connection did not survive {} without traffic (a NAT or firewall \
                         probably dropped it); the next request reconnected in {}.",
                        fmt_ms(i.idle_ms),
                        i.recovery_ms.map_or("-".into(), fmt_ms)
                    ),
                ));
            } else if i.path_before == "direct" && i.path_after != "direct" {
                hints.push(finding(
                    "idle_path_lost",
                    format!(
                        "The direct path was lost during {} without traffic (the connection \
                         continued over the relay); a NAT probably dropped its mapping.",
                        fmt_ms(i.idle_ms)
                    ),
                ));
            }
        }
        // Only the latency phase counts: its few small packets cannot
        // congest any link, so what is lost there is lost by the path. In the
        // transfers, congestion control loses packets by design while it
        // probes for the link's capacity.
        if let Some(q) = p.latency.as_ref().and_then(|l| l.quic.as_ref())
            && let Some(pct) = q.lost_pct
            && q.lost_packets > 0
            && q.sent_packets >= LOSS_MIN_PACKETS
            && pct >= LOSS_HINT_PCT
        {
            hints.push(finding(
                "packet_loss",
                format!(
                    "{} of the {} packets this node sent while measuring latency were lost \
                     ({pct:.1}%). The connection was nearly idle then, so the network lost them \
                     (for example Wi-Fi, a mobile or an overloaded link), not congestion from \
                     this test. Random loss like this slows every transfer down: about 1% can \
                     cut the throughput of a connection by ten times or more.",
                    q.lost_packets, q.sent_packets
                ),
            ));
        }
        if let Some(l) = &p.latency
            && let (Some(j), Some(m)) = (l.jitter_ms, l.rtt_ms.p50)
            && j > 30.0
            && j > m / 2.0
        {
            hints.push(finding(
                "jitter",
                format!(
                    "Round-trip times vary a lot (jitter {}, median {}), as is typical for \
                     Wi-Fi or a busy uplink.",
                    fmt_ms(j),
                    fmt_ms(m)
                ),
            ));
        }
        if let (Some(idle), Some(loaded)) = (
            p.latency.as_ref().and_then(|l| l.rtt_ms.p50),
            p.concurrency.as_ref().and_then(|c| c.loaded_rtt_ms.p50),
        ) && loaded > 3.0 * idle
            && loaded - idle > 100.0
        {
            hints.push(finding(
                "latency_under_load",
                format!(
                    "While the connection was busy, small requests took {} instead of {} \
                     (median): large transfers delay other traffic on this link.",
                    fmt_ms(loaded),
                    fmt_ms(idle)
                ),
            ));
        }
        if let Some(rc) = &self.r.relay_compare
            && rc.skipped.is_none()
        {
            if !rc.ok {
                let err = rc
                    .connect
                    .as_ref()
                    .and_then(|c| c.error.clone())
                    .or_else(|| {
                        [&rc.upload, &rc.download]
                            .into_iter()
                            .flatten()
                            .find_map(|t| t.error.clone())
                    })
                    .unwrap_or_else(|| "requests failed".into());
                issues.push(finding(
                    "relay_fallback_failed",
                    format!(
                        "The relay-only connection to the peer did not work ({err}). If hole \
                         punching fails, this peer cannot be reached."
                    ),
                ));
            } else if self.direct_seen {
                hints.push(finding(
                    "relay_fallback",
                    format!(
                        "If hole punching fails, traffic goes through the relay{}: {} (direct: \
                         {}).",
                        rc.relay
                            .as_ref()
                            .map(|r| format!(" {r}"))
                            .unwrap_or_default(),
                        perf(
                            rc.latency.as_ref(),
                            rc.upload.as_ref(),
                            rc.download.as_ref()
                        ),
                        perf(p.latency.as_ref(), p.upload.as_ref(), p.download.as_ref()),
                    ),
                ));
            }
        }
        if !self.direct_seen && !self.relay_only_mode {
            unknowns.push(unknown(
                "peer_network",
                "the peer's NAT and firewall cannot be seen from this side",
            ));
        }
        if p.quic.is_some() || p.latency.as_ref().is_some_and(|l| l.quic.is_some()) {
            unknowns.push(unknown(
                "download_loss",
                "packet loss is counted only for packets this node sent (upload direction)",
            ));
        }
    }

    /// The verdict sentence(s), and the code of a finding they already
    /// explain.
    fn headline(&self, status: Status) -> (String, Option<&'static str>) {
        let name = self.peer_name();
        let Some(p) = self.peer else {
            let text = match status {
                Status::Healthy => {
                    let mut parts = Vec::new();
                    if let Some(n) = self.nr
                        && (n.udp_v4 || n.udp_v6)
                    {
                        parts.push("UDP works".to_string());
                        if let Some(a) = &n.public_v4 {
                            parts.push(format!("public address {a}"));
                        }
                    }
                    if self.reachable_relays > 0 {
                        parts.push(format!(
                            "{} relay{} reachable",
                            self.reachable_relays,
                            if self.reachable_relays == 1 { "" } else { "s" }
                        ));
                    }
                    if self.r.local.as_ref().and_then(|l| l.record_published) == Some(true) {
                        parts.push("your record is published".into());
                    }
                    if parts.is_empty() {
                        "No problems found.".into()
                    } else {
                        format!(
                            "Your network looks fine for golem-p2p: {}.",
                            parts.join(", ")
                        )
                    }
                }
                _ => "Problems found on your network, see below.".to_string(),
            };
            return (text, None);
        };
        let perf = perf(p.latency.as_ref(), p.upload.as_ref(), p.download.as_ref());
        let perf = if perf.is_empty() {
            String::new()
        } else {
            format!(": {perf}")
        };
        let mut covered = None;
        let text = match status {
            Status::NoConnection => format!(
                "Could not connect to {name}: {}.",
                self.connect_reason(&p.connect.error_kind, p.connect.error.as_deref())
            ),
            Status::RelayOnly => {
                let via = p
                    .path
                    .final_remote_addr
                    .as_ref()
                    .filter(|_| p.path.final_path == "relay")
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                let (reason, code) = self.relay_reason();
                covered = code;
                format!("Connected to {name}, but only through a relay{via}{perf}. {reason}")
            }
            Status::Degraded => {
                // The path at the end, else the last one seen (no path is
                // selected for a moment, or the connection is gone).
                let last = match p.path.final_path {
                    "unknown" => p.path.timeline.last().map_or("unknown", |e| e.path),
                    known => known,
                };
                let how = match last {
                    "direct" => "direct",
                    "relay" => "through a relay",
                    _ => "path unknown",
                };
                let lost = p.connection_lost.as_ref().filter(|c| c.phase != "idle");
                if lost.is_some() && p.path.final_path == "unknown" {
                    format!("Connected to {name} ({how}), but the connection was lost{perf}.")
                } else if lost.is_some() {
                    format!(
                        "Connected to {name} ({how}), but the connection was lost once and had \
                         to be made again{perf}."
                    )
                } else {
                    format!("Connected to {name} ({how}), but with problems{perf}.")
                }
            }
            Status::Healthy => {
                let punched = match (p.path.timeline.first(), p.path.time_to_direct_ms) {
                    (Some(first), Some(t)) if first.path != "direct" => {
                        format!(" (hole punching took {})", fmt_ms(t))
                    }
                    _ => String::new(),
                };
                format!("Good: direct connection to {name}{punched}{perf}.")
            }
        };
        (text, covered)
    }

    fn connect_reason(&self, kind: &Option<&'static str>, error: Option<&str>) -> String {
        let error = error.unwrap_or("unknown error");
        match kind.unwrap_or("") {
            "not_found" => "it is not in any directory (the node is not running, has not \
                            published its record yet, or uses another directory)"
                .into(),
            "timeout" | "connect" => {
                let mut s = format!(
                    "its record was found, but it did not answer within {} (it may be offline, \
                     or a firewall blocks the way)",
                    self.timeout()
                );
                if self.r.local.as_ref().is_some_and(|l| !l.relays.is_empty())
                    && self.reachable_relays == 0
                {
                    s += "; none of your relays is reachable either";
                }
                s
            }
            "rejected" | "handshake" | "not_peer_identity" => {
                format!("the node refused the connection ({error})")
            }
            "discovery" => format!("the directory lookup failed ({error})"),
            _ => error.to_string(),
        }
    }

    /// Why there was no direct path, and the finding that says the same.
    fn relay_reason(&self) -> (&'static str, Option<&'static str>) {
        if self.relay_only_mode {
            ("This run used --relay-only.", None)
        } else if self.no_udp_answer {
            (
                "UDP seems to be blocked on your network (no relay answered over UDP), so all \
                 traffic goes through a relay. Ask your network admin to allow outgoing UDP.",
                Some("udp_blocked"),
            )
        } else if self.hard_nat {
            (
                "Your NAT changes ports per destination (hard NAT); direct connections to \
                 peers behind similar NATs fall back to relays.",
                Some("hard_nat"),
            )
        } else if let Some(n) = self.nr
            && (n.udp_v4 || n.udp_v6)
        {
            if n.mapping_varies_by_dest == Some(false) {
                (
                    "Your side looks fine (UDP works, your NAT keeps its port mapping), so the \
                     peer's network probably blocks UDP or uses a hard NAT.",
                    None,
                )
            } else {
                (
                    "UDP works on your side; the peer's network may block UDP, or one of the \
                     two NATs prevents hole punching.",
                    None,
                )
            }
        } else {
            (
                "Why no direct path was found is unknown (UDP was not measured).",
                None,
            )
        }
    }
}

/// What a relay check failure means for the user.
fn relay_advice(c: &RelayCheck) -> String {
    match c.error_kind.unwrap_or("") {
        "tls_untrusted" => {
            " If it uses a self-signed certificate, pass that certificate with --relay-ca.".into()
        }
        "tls_certificate" => " The relay operator has to fix its certificate.".into(),
        "dns" => " Check the host name and your DNS.".into(),
        "tcp" | "timeout" => {
            " The relay is down, or a firewall blocks the connection to it.".into()
        }
        "http" => " It answered, but not like an iroh relay; check the URL.".into(),
        _ => String::new(),
    }
}

/// `22 ms round trip, 38.1 Mbps up, 42.0 Mbps down` (known parts only).
fn perf(
    latency: Option<&Latency>,
    up: Option<&TransferReport>,
    down: Option<&TransferReport>,
) -> String {
    let mut v = Vec::new();
    if let Some(m) = latency.and_then(|l| l.rtt_ms.p50) {
        v.push(format!("{} round trip", fmt_ms(m)));
    }
    if let Some(u) = up.and_then(|t| t.throughput_mbps) {
        v.push(format!("{} up", fmt_mbps(u)));
    }
    if let Some(d) = down.and_then(|t| t.throughput_mbps) {
        v.push(format!("{} down", fmt_mbps(d)));
    }
    v.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bench::report::{
            Concurrency, Connect, ConnectionLost, DirectoryCheck, HomeRelay, Local, PathPoint,
            PathReport, QuicStats, RelayCompare, Settings,
        },
        probe::Stats,
    };

    fn settings() -> Settings {
        Settings {
            timeout_ms: 10_000.0,
            latency_duration_ms: 10_000.0,
            interval_ms: 200.0,
            payload_bytes: 64,
            transfer_bytes: 10_000_000,
            transfer_time_ms: 8000.0,
            relay_transfer_bytes: 4_000_000,
            streams: 8,
            concurrency_bytes: 8_000_000,
            concurrency_request_bytes: 262_144,
            compare_relay: false,
            idle_ms: None,
            skip: vec![],
        }
    }

    /// A healthy local check: one relay with QAD, UDP works, record published.
    fn local() -> Local {
        Local {
            duration_ms: 1000.0,
            home_relay: Some(HomeRelay {
                url: Some("https://r1/".into()),
                connected: true,
                error: None,
            }),
            net_report: Some(NetReport {
                udp_v4: true,
                public_v4: Some("1.2.3.4:5000".into()),
                qad_relays_v4: 1,
                ..Default::default()
            }),
            relays: vec![RelayCheck {
                url: "https://r1/".into(),
                quic_port: Some(7842),
                ok: true,
                ..Default::default()
            }],
            directories: vec![DirectoryCheck {
                url: "http://dir/".into(),
                ok: true,
                lookup_ms: Some(3.0),
                own_record: Some("published"),
                error: None,
            }],
            record_published: Some(true),
            ..Default::default()
        }
    }

    fn report(local: Option<Local>, peer: Option<Peer>) -> Report {
        Report {
            status: Status::Healthy,
            exit_code: 0,
            verdict: String::new(),
            issues: vec![],
            hints: vec![],
            unknowns: vec![],
            node_id: "0x00a329c0648769a73afac7f9381e08fb43dbea72".into(),
            endpoint_id: "ab".repeat(32),
            temporary_identity: true,
            transport: "direct_and_relay",
            to: peer
                .as_ref()
                .map(|_| "0x1111111111111111111111111111111111111111".into()),
            duration_ms: 1.0,
            interrupted: false,
            settings: settings(),
            local,
            peer,
            relay_compare: None,
            in_verdict: vec![],
        }
    }

    fn latency(p50: f64) -> Latency {
        Latency {
            sent: 50,
            ok: 50,
            rtt_ms: Stats {
                min: Some(p50),
                p50: Some(p50),
                p95: Some(p50),
                max: Some(p50),
                mean: Some(p50),
            },
            rtt_samples: 50,
            final_path: "direct",
            ..Default::default()
        }
    }

    fn transfer(mbps: f64) -> TransferReport {
        TransferReport {
            ok: true,
            requested_bytes: 1000,
            bytes: Some(1000),
            duration_ms: Some(1.0),
            throughput_mbps: Some(mbps),
            path: Some("direct"),
            limited_by: Some("size"),
            ..Default::default()
        }
    }

    /// A peer that connected; `direct`: a direct path was seen.
    fn peer(direct: bool) -> Peer {
        let path = if direct { "direct" } else { "relay" };
        Peer {
            connect: Connect {
                ok: true,
                connect_ms: Some(300.0),
                path: "relay",
                ..Default::default()
            },
            latency: Some(latency(22.0)),
            upload: Some(transfer(38.1)),
            download: Some(transfer(42.0)),
            concurrency: None,
            idle: None,
            path: PathReport {
                time_to_direct_ms: direct.then_some(1200.0),
                changes: u64::from(direct),
                final_path: path,
                final_remote_addr: Some(if direct {
                    "1.2.3.4:5".into()
                } else {
                    "https://r1/".into()
                }),
                timeline: vec![PathPoint {
                    t_ms: 0.0,
                    path: "relay",
                    remote_addr: None,
                }],
            },
            quic: None,
            connection_lost: None,
        }
    }

    fn codes(f: &[Finding]) -> Vec<&'static str> {
        f.iter().map(|f| f.code).collect()
    }

    #[test]
    fn local_only_healthy() {
        let mut r = report(Some(local()), None);
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::Healthy, 0), "{r:#?}");
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(r.verdict.contains("UDP works"), "{}", r.verdict);
        assert!(r.verdict.contains("1.2.3.4:5000"), "{}", r.verdict);
        // Port mapping and the NAT mapping (one relay) are not measured.
        let items: Vec<_> = r.unknowns.iter().map(|u| u.item).collect();
        assert_eq!(items, ["port_mapping", "nat_mapping"]);
        // Without a NAT there is no NAT mapping to measure.
        let mut l = local();
        l.net_report.as_mut().unwrap().behind_nat_v4 = Some(false);
        let mut r = report(Some(l), None);
        analyze(&mut r);
        let items: Vec<_> = r.unknowns.iter().map(|u| u.item).collect();
        assert_eq!(items, ["port_mapping"]);
    }

    #[test]
    fn direct_peer_healthy() {
        let mut r = report(Some(local()), Some(peer(true)));
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy, "{r:#?}");
        assert_eq!(
            r.verdict,
            "Good: direct connection to 0x1111...1111 (hole punching took 1.2 s): \
             22 ms round trip, 38.1 Mbps up, 42.0 Mbps down."
        );
    }

    #[test]
    fn udp_blocked_relay_only() {
        let mut l = local();
        l.net_report = Some(NetReport::default());
        let mut r = report(Some(l), Some(peer(false)));
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::RelayOnly, 4));
        assert!(
            r.verdict.contains("only through a relay (https://r1/)"),
            "{}",
            r.verdict
        );
        assert!(
            r.verdict.contains("UDP seems to be blocked"),
            "{}",
            r.verdict
        );
        assert!(r.verdict.contains("allow outgoing UDP"), "{}", r.verdict);
        assert_eq!(codes(&r.issues), ["udp_blocked"]);
        // The verdict says it already; the human report does not repeat it.
        assert_eq!(r.in_verdict, ["udp_blocked"]);

        // The same without a peer: degraded, not "relay only".
        let mut l = local();
        l.net_report = Some(NetReport::default());
        let mut r = report(Some(l), None);
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::Degraded, 5));
    }

    #[test]
    fn qad_failure_with_direct_path_is_not_udp_blocked() {
        let mut l = local();
        l.net_report = Some(NetReport::default());
        let mut r = report(Some(l), Some(peer(true)));
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy);
        assert_eq!(codes(&r.hints), ["qad_failed"]);
    }

    #[test]
    fn no_udp_claim_without_qad_or_reachable_relay() {
        // QAD off: no claim about UDP.
        let mut l = local();
        l.relays[0].quic_port = None;
        l.net_report = Some(NetReport::default());
        let mut r = report(Some(l), Some(peer(false)));
        analyze(&mut r);
        assert_eq!(r.status, Status::RelayOnly);
        assert!(!r.verdict.contains("blocked"), "{}", r.verdict);
        assert!(
            r.unknowns.iter().any(|u| u.item == "udp"),
            "{:?}",
            r.unknowns
        );
        // Relay unreachable over HTTPS too: that is the problem reported.
        let mut l = local();
        l.relays[0].ok = false;
        l.relays[0].error_kind = Some("tls_untrusted");
        l.relays[0].error = Some("not trusted".into());
        l.net_report = Some(NetReport::default());
        let mut r = report(Some(l), None);
        analyze(&mut r);
        assert_eq!(codes(&r.issues), ["relay_unreachable"]);
        assert!(r.issues[0].message.contains("--relay-ca"), "{:?}", r.issues);
    }

    #[test]
    fn hard_nat() {
        let mut l = local();
        let n = l.net_report.as_mut().unwrap();
        n.mapping_varies_by_dest = Some(true);
        n.qad_relays_v4 = 2;
        let mut r = report(Some(l), Some(peer(false)));
        analyze(&mut r);
        assert_eq!(r.status, Status::RelayOnly);
        assert!(r.verdict.contains("hard NAT"), "{}", r.verdict);
        assert_eq!(codes(&r.hints), ["hard_nat"]);
        // UDP works and the NAT is fine: the peer side is the likely cause.
        let mut l = local();
        l.net_report.as_mut().unwrap().mapping_varies_by_dest = Some(false);
        let mut r = report(Some(l), Some(peer(false)));
        analyze(&mut r);
        assert!(
            r.verdict.contains("peer's network probably"),
            "{}",
            r.verdict
        );
    }

    #[test]
    fn not_found() {
        let mut p = peer(false);
        p.connect = Connect {
            ok: false,
            path: "unknown",
            error_kind: Some("not_found"),
            error: Some("no valid record found".into()),
            ..Default::default()
        };
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::NoConnection, 3));
        assert!(r.verdict.contains("not in any directory"), "{}", r.verdict);
    }

    #[test]
    fn degraded_by_refused_transfer_and_failures() {
        let mut p = peer(true);
        p.upload = Some(TransferReport {
            ok: false,
            requested_bytes: 1000,
            error_kind: Some("refused"),
            error: Some("the listener refused the transfer".into()),
            ..Default::default()
        });
        p.latency.as_mut().unwrap().failed = 2;
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::Degraded, 5));
        assert_eq!(codes(&r.issues), ["requests_failed", "transfer_refused"]);
        assert!(r.verdict.contains("but with problems"), "{}", r.verdict);
    }

    /// Direct until the connection was closed in the concurrency phase, as on
    /// the lossy link of campaign 1 (runs 2 and 3).
    fn lost_direct_peer() -> Peer {
        let mut p = peer(true);
        p.path.timeline.push(PathPoint {
            t_ms: 1200.0,
            path: "direct",
            remote_addr: Some("1.2.3.4:5".into()),
        });
        p.path.final_path = "unknown";
        p.path.final_remote_addr = None;
        p.concurrency = Some(Concurrency {
            streams: 8,
            failed: 8,
            error: Some("timed out".into()),
            ..Default::default()
        });
        p.connection_lost = Some(ConnectionLost {
            t_ms: 52_000.0,
            phase: "concurrency",
            cause: "closed_here",
            reason: "closed".into(),
        });
        p
    }

    #[test]
    fn lost_connection_is_not_called_relayed() {
        let mut r = report(Some(local()), Some(lost_direct_peer()));
        analyze(&mut r);
        assert_eq!((r.status, r.exit_code), (Status::Degraded, 5));
        assert!(!r.verdict.contains("relay"), "{}", r.verdict);
        assert!(
            r.verdict
                .starts_with("Connected to 0x1111...1111 (direct), but the connection was lost:"),
            "{}",
            r.verdict
        );
        assert_eq!(codes(&r.issues), ["parallel_failed", "connection_lost"]);
        let m = &r.issues[1].message;
        assert!(
            m.contains("after 52 s, during the concurrency phase"),
            "{m}"
        );
        assert!(
            m.contains("nothing at all arrived from the peer for 10 s"),
            "{m}"
        );
        assert!(m.ends_with("at the end there was no connection."), "{m}");

        // Reconnected by a later request: said so.
        let mut p = lost_direct_peer();
        p.path.final_path = "direct";
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert!(
            r.verdict
                .contains("(direct), but the connection was lost once")
        );
        assert!(
            r.issues[1]
                .message
                .ends_with("a later request connected again.")
        );

        // Problems without a lost connection: the last path seen, not "relay"
        // for an unknown one.
        let mut p = lost_direct_peer();
        p.connection_lost = None;
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert!(
            r.verdict.contains("(direct), but with problems"),
            "{}",
            r.verdict
        );

        // Lost while idle on purpose: that is the idle finding's business.
        let mut p = peer(true);
        p.connection_lost = Some(ConnectionLost {
            phase: "idle",
            cause: "idle_timeout",
            ..Default::default()
        });
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy, "{:?}", r.issues);
    }

    fn quic(sent: u64, lost: u64) -> QuicStats {
        QuicStats {
            sent_packets: sent,
            lost_packets: lost,
            lost_pct: Some(lost as f64 * 100.0 / sent as f64),
            ..Default::default()
        }
    }

    #[test]
    fn packet_loss_only_from_the_latency_phase() {
        // Congestion losses of the transfers on a fast, clean path (campaign
        // 1: 2.3-6.8% at 135-149 Mbps) are not a problem of the path.
        let mut p = peer(true);
        p.latency.as_mut().unwrap().quic = Some(quic(112, 0));
        p.upload.as_mut().unwrap().quic = Some(quic(8000, 400));
        p.quic = Some(quic(9000, 400));
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy, "{r:#?}");
        assert!(r.hints.is_empty(), "{:?}", r.hints);

        // About 1% lost while the connection was nearly idle (the lab's
        // lossy link: 1% each way) is flagged, whatever the transfers lost.
        let mut p = peer(true);
        p.latency.as_mut().unwrap().quic = Some(quic(112, 1));
        p.upload.as_mut().unwrap().quic = Some(quic(8000, 80));
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy);
        assert_eq!(codes(&r.hints), ["packet_loss"]);
        assert!(
            r.hints[0].message.contains("1 of the 112 packets"),
            "{}",
            r.hints[0].message
        );

        // Below the threshold (0.4%), or too few packets to say: no hint.
        for q in [quic(250, 1), quic(30, 1)] {
            let mut p = peer(true);
            p.latency.as_mut().unwrap().quic = Some(q);
            let mut r = report(Some(local()), Some(p));
            analyze(&mut r);
            assert!(r.hints.is_empty(), "{:?}", r.hints);
        }
        // Without counters for the latency phase the whole connection's loss
        // is not used either.
        let mut p = peer(true);
        p.quic = Some(quic(9000, 900));
        let mut r = report(Some(local()), Some(p));
        analyze(&mut r);
        assert!(r.hints.is_empty(), "{:?}", r.hints);
    }

    #[test]
    fn relay_fallback() {
        let mut r = report(Some(local()), Some(peer(true)));
        r.relay_compare = Some(RelayCompare {
            ok: true,
            relay: Some("https://r1/".into()),
            latency: Some(latency(75.0)),
            upload: Some(transfer(22.0)),
            download: Some(transfer(20.0)),
            ..Default::default()
        });
        analyze(&mut r);
        assert_eq!(r.status, Status::Healthy);
        let h = r.hints.iter().find(|h| h.code == "relay_fallback").unwrap();
        assert!(
            h.message.contains("75 ms round trip, 22.0 Mbps up"),
            "{}",
            h.message
        );
        // A broken fallback is a problem.
        r.relay_compare.as_mut().unwrap().ok = false;
        analyze(&mut r);
        assert_eq!(r.status, Status::Degraded);
        assert_eq!(codes(&r.issues), ["relay_fallback_failed"]);
    }
}
