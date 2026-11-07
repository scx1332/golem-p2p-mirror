//! The bench report: the `bench_report` event of `bench --json`, and its
//! human-readable form. Field names are stable (see `docs/cli-events.md`);
//! units are part of the names, unknown values are null.

use serde::Serialize;

use crate::probe::Stats;

/// The overall result; decides the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Everything measured is fine.
    Healthy,
    /// Could not connect to the peer at all.
    NoConnection,
    /// Connected, but only through a relay.
    RelayOnly,
    /// Connected (or local check only), but problems were found.
    Degraded,
}

impl Status {
    pub fn exit_code(self) -> u8 {
        match self {
            Status::Healthy => 0,
            Status::NoConnection => 3,
            Status::RelayOnly => 4,
            Status::Degraded => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Status::Healthy => "healthy",
            Status::NoConnection => "no connection",
            Status::RelayOnly => "relay only",
            Status::Degraded => "degraded",
        }
    }
}

/// A problem (in `issues`) or an observation with advice (in `hints`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable machine-readable code, e.g. `udp_blocked`.
    pub code: &'static str,
    /// Plain-language explanation.
    pub message: String,
}

/// Something the bench could not determine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unknown {
    /// Stable name, e.g. `port_mapping`.
    pub item: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: Status,
    pub exit_code: u8,
    /// One or two sentences in plain language.
    pub verdict: String,
    pub issues: Vec<Finding>,
    pub hints: Vec<Finding>,
    pub unknowns: Vec<Unknown>,
    pub node_id: String,
    pub endpoint_id: String,
    /// No `--key`: a temporary identity was used.
    pub temporary_identity: bool,
    /// `direct_and_relay`, `relay_only` or `direct_only`.
    pub transport: &'static str,
    pub to: Option<String>,
    pub duration_ms: f64,
    /// Ended early by SIGINT/SIGTERM.
    pub interrupted: bool,
    pub settings: Settings,
    pub local: Option<Local>,
    pub peer: Option<Peer>,
    pub relay_compare: Option<RelayCompare>,
    /// Codes of findings that the verdict already explains; the human
    /// report does not repeat them.
    #[serde(skip)]
    pub in_verdict: Vec<&'static str>,
}

/// The effective parameters of the run.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub timeout_ms: f64,
    pub latency_duration_ms: f64,
    pub interval_ms: f64,
    pub payload_bytes: usize,
    pub transfer_bytes: u64,
    pub transfer_time_ms: f64,
    pub relay_transfer_bytes: u64,
    pub streams: usize,
    pub concurrency_bytes: u64,
    pub concurrency_request_bytes: usize,
    pub compare_relay: bool,
    pub idle_ms: Option<f64>,
    pub skip: Vec<&'static str>,
}

/// Part A: the local network check.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Local {
    pub duration_ms: f64,
    /// Local UDP sockets of the endpoint.
    pub bound_sockets: Vec<String>,
    /// Direct addresses as published in the node's record, best first: the
    /// public address from QUIC address discovery and the addresses of local
    /// interfaces, without those of container and VM bridges.
    pub direct_addrs: Vec<String>,
    pub home_relay: Option<HomeRelay>,
    /// iroh's net report; null without one (no relays, or not in time).
    pub net_report: Option<NetReport>,
    pub relays: Vec<RelayCheck>,
    pub directories: Vec<DirectoryCheck>,
    /// Our current record is in at least one directory (true), in none of
    /// the reachable ones (false), or no directory answered (null).
    pub record_published: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HomeRelay {
    pub url: Option<String>,
    pub connected: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetReport {
    /// A relay answered QUIC address discovery over IPv4 (UDP works).
    pub udp_v4: bool,
    pub udp_v6: bool,
    /// Our address as seen by the relays (`ip:port`).
    pub public_v4: Option<String>,
    pub public_v6: Option<String>,
    /// The public IPv4 address is not one of this machine's addresses.
    pub behind_nat_v4: Option<bool>,
    /// The public IPv4 port equals the local port.
    pub port_preserved_v4: Option<bool>,
    /// Relays saw different public addresses (hard NAT). Null unless at
    /// least two relays answered over the same IP version.
    pub mapping_varies_by_dest: Option<bool>,
    /// Relays that answered QUIC address discovery.
    pub qad_relays_v4: usize,
    pub qad_relays_v6: usize,
    pub preferred_relay: Option<String>,
    /// iroh's captive portal check (plain HTTP to port 80 of a relay host);
    /// only meaningful if the relay serves port 80.
    pub captive_portal: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RelayCheck {
    pub url: String,
    /// QUIC address discovery port; null if off.
    pub quic_port: Option<u16>,
    /// The QAD setting was given (`,quic=` or `--relay-quic-port`); false
    /// for iroh's default port.
    pub quic_port_explicit: bool,
    /// DNS, TCP, TLS (certificate verified) and `GET /ping` all succeeded.
    pub ok: bool,
    /// The address connected to.
    pub addr: Option<String>,
    pub dns_ms: Option<f64>,
    pub tcp_ms: Option<f64>,
    pub tls_ms: Option<f64>,
    pub http_ms: Option<f64>,
    pub http_status: Option<u16>,
    /// From iroh's net report: HTTPS latency, and QUIC address discovery
    /// round trips over IPv4 and IPv6 (null: no answer).
    pub https_latency_ms: Option<f64>,
    pub qad_v4_ms: Option<f64>,
    pub qad_v6_ms: Option<f64>,
    /// `dns`, `tcp`, `tls_untrusted`, `tls_certificate`, `tls`, `http` or
    /// `timeout`.
    pub error_kind: Option<&'static str>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DirectoryCheck {
    pub url: String,
    /// The directory answered the lookup of our own node ID.
    pub ok: bool,
    pub lookup_ms: Option<f64>,
    /// `published` (our endpoint), `missing`, or `other_endpoint` (another
    /// process published a newer record for our identity).
    pub own_record: Option<&'static str>,
    pub error: Option<String>,
}

/// Part B: the peer benchmark.
#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    pub connect: Connect,
    pub latency: Option<Latency>,
    pub upload: Option<TransferReport>,
    pub download: Option<TransferReport>,
    pub concurrency: Option<Concurrency>,
    pub idle: Option<Idle>,
    pub path: PathReport,
    pub quic: Option<QuicStats>,
    /// The connection made in the connect phase closed before the end of
    /// the run; null if it stayed open.
    pub connection_lost: Option<ConnectionLost>,
}

/// When and why the bench's connection to the peer closed during the run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConnectionLost {
    /// Since the start of connecting.
    pub t_ms: f64,
    /// The phase that was running: `latency`, `upload`, `download`,
    /// `concurrency`, `relay_compare` or `idle`.
    pub phase: &'static str,
    /// `closed_here` (this node closed it: a request got no answer and
    /// nothing at all arrived from the peer during the request timeout),
    /// `idle_timeout` (nothing arrived from the peer for QUIC's idle
    /// timeout), `closed_by_peer`, `reset` (the peer no longer knew the
    /// connection, e.g. it restarted) or `error`.
    pub cause: &'static str,
    /// QUIC's close reason.
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Connect {
    pub ok: bool,
    /// Directory lookup of the peer's record.
    pub lookup_ms: Option<f64>,
    /// Lookup, QUIC/TLS handshake and golem-p2p handshake.
    pub connect_ms: Option<f64>,
    /// `connect_ms - lookup_ms`.
    pub handshake_ms: Option<f64>,
    /// Path right after connecting.
    pub path: &'static str,
    pub remote_addr: Option<String>,
    pub peer_endpoint: Option<String>,
    pub error_kind: Option<&'static str>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Latency {
    pub duration_ms: f64,
    pub sent: u64,
    pub ok: u64,
    pub failed: u64,
    /// `failed / sent * 100`.
    pub failed_pct: Option<f64>,
    pub rtt_ms: Stats,
    pub rtt_samples: usize,
    pub jitter_ms: Option<f64>,
    /// Fraction of correct replies that came over a direct path.
    pub direct_fraction: Option<f64>,
    pub final_path: &'static str,
    /// QUIC counters of the connection during this phase; null if the
    /// connection changed meanwhile.
    pub quic: Option<QuicStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TransferReport {
    pub ok: bool,
    /// The size asked for (after the relay cap).
    pub requested_bytes: u64,
    /// The size was capped by `--relay-transfer-size` (relayed path).
    pub relay_capped: bool,
    pub bytes: Option<u64>,
    pub duration_ms: Option<f64>,
    pub throughput_mbps: Option<f64>,
    pub path: Option<&'static str>,
    /// `size` (all bytes) or `time` (`--transfer-time` reached first).
    pub limited_by: Option<&'static str>,
    pub error_kind: Option<&'static str>,
    pub error: Option<String>,
    /// QUIC counters of the connection during this phase.
    pub quic: Option<QuicStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Concurrency {
    pub ok: bool,
    pub streams: usize,
    pub request_bytes: usize,
    pub requests: u64,
    pub failed: u64,
    /// Payload bytes of the correct replies; each went both ways.
    pub total_bytes: u64,
    pub duration_ms: f64,
    /// Payload throughput in each direction.
    pub throughput_mbps: Option<f64>,
    pub rtt_ms: Stats,
    /// Small requests (`--payload-size`) sent meanwhile.
    pub loaded_rtt_ms: Stats,
    pub loaded_samples: usize,
    pub error_kind: Option<&'static str>,
    pub error: Option<String>,
    /// QUIC counters of the connection during this phase.
    pub quic: Option<QuicStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Idle {
    pub idle_ms: f64,
    pub ok: bool,
    /// The connection from before the idle time was still open after it.
    pub survived: bool,
    pub path_before: &'static str,
    pub path_after: &'static str,
    pub attempts: u64,
    /// From the first request after the idle time to the first reply.
    pub recovery_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PathReport {
    /// From the start of connecting until a direct path was first seen.
    pub time_to_direct_ms: Option<f64>,
    pub changes: u64,
    pub final_path: &'static str,
    pub final_remote_addr: Option<String>,
    /// Selected paths over time (at most 64).
    pub timeline: Vec<PathPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathPoint {
    /// Since the start of connecting.
    pub t_ms: f64,
    pub path: &'static str,
    pub remote_addr: Option<String>,
}

/// QUIC counters of the connection (this side's view), for the whole run or
/// one phase.
#[derive(Debug, Clone, Default, Serialize)]
pub struct QuicStats {
    /// Packets this node sent, without path MTU probes.
    pub sent_packets: u64,
    /// Packets this node sent that were declared lost (and resent), without
    /// path MTU probes: those are lost by design when they are larger than
    /// the path allows.
    pub lost_packets: u64,
    pub lost_pct: Option<f64>,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}

/// The same peer over a relay-only connection (`--compare-relay`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RelayCompare {
    /// Why it did not run; null if it ran.
    pub skipped: Option<String>,
    pub ok: bool,
    /// The relay the connection went through.
    pub relay: Option<String>,
    pub connect: Option<Connect>,
    pub latency: Option<Latency>,
    pub upload: Option<TransferReport>,
    pub download: Option<TransferReport>,
    pub quic: Option<QuicStats>,
}

/// `1.2 s`, `350 ms`, `4.5 ms`.
pub fn fmt_ms(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.0} s", v / 1000.0)
    } else if v >= 1000.0 {
        format!("{:.1} s", v / 1000.0)
    } else if v >= 10.0 {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

/// `10.0 MB`, `512 kB`, `64 B` (decimal units).
pub fn fmt_bytes(v: u64) -> String {
    let f = v as f64;
    if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} kB", f / 1e3)
    } else {
        format!("{v} B")
    }
}

/// `38.1 Mbps`, `850 kbps`.
pub fn fmt_mbps(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0} Mbps")
    } else if v >= 1.0 {
        format!("{v:.1} Mbps")
    } else {
        format!("{:.0} kbps", v * 1000.0)
    }
}

fn opt_ms(v: Option<f64>) -> String {
    v.map_or("-".into(), fmt_ms)
}

/// A short form of a node ID for prose: `0x1234...abcd`.
pub fn short_id(id: &str) -> String {
    if id.len() > 14 {
        format!("{}...{}", &id[..6], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

impl Report {
    /// Prints the human-readable report on stdout.
    pub fn print_human(&self) {
        let mut s = String::new();
        self.write_human(&mut s);
        print!("{s}");
    }

    fn write_human(&self, s: &mut String) {
        use std::fmt::Write;
        let line = |s: &mut String, k: &str, v: String| {
            let _ = writeln!(s, "  {k:<15} {v}");
        };
        let _ = writeln!(s, "\ngolem-p2p bench");
        line(
            s,
            "this node",
            format!(
                "{}{} (endpoint {}, {})",
                self.node_id,
                if self.temporary_identity {
                    ", temporary identity"
                } else {
                    ""
                },
                &self.endpoint_id[..self.endpoint_id.len().min(10)],
                self.transport.replace('_', " "),
            ),
        );
        if let Some(to) = &self.to {
            line(s, "peer", to.clone());
        }
        if self.interrupted {
            line(s, "note", "interrupted, results are incomplete".into());
        }

        if let Some(l) = &self.local {
            let _ = writeln!(s, "\nLocal network ({})", fmt_ms(l.duration_ms));
            for r in &l.relays {
                let v = if r.ok {
                    let mut parts = Vec::new();
                    if let Some(d) = r.dns_ms {
                        parts.push(format!("DNS {}", fmt_ms(d)));
                    }
                    parts.push(format!("TCP {}", opt_ms(r.tcp_ms)));
                    parts.push(format!("TLS {}", opt_ms(r.tls_ms)));
                    parts.push(format!("HTTPS {}", opt_ms(r.http_ms)));
                    let udp = match (r.quic_port, r.qad_v4_ms.or(r.qad_v6_ms)) {
                        (None, _) => "QUIC address discovery off".to_string(),
                        (Some(_), Some(q)) => format!("UDP {}", fmt_ms(q)),
                        (Some(p), None) => format!("no UDP answer (port {p})"),
                    };
                    format!("ok: {}; {udp}", parts.join(", "))
                } else {
                    format!(
                        "NOT REACHABLE: {}",
                        r.error.as_deref().unwrap_or("unknown error")
                    )
                };
                line(s, "relay", format!("{}  {v}", r.url));
            }
            if l.relays.is_empty() {
                line(s, "relay", "none configured".into());
            }
            if let Some(h) = &l.home_relay {
                let v = match (&h.url, h.connected) {
                    (Some(u), true) => format!("{u} (connected)"),
                    (Some(u), false) => format!(
                        "{u} (NOT connected{})",
                        h.error
                            .as_ref()
                            .map(|e| format!(": {e}"))
                            .unwrap_or_default()
                    ),
                    (None, _) => "none".into(),
                };
                line(s, "home relay", v);
            }
            match &l.net_report {
                Some(n) => {
                    let udp = match (n.udp_v4, n.udp_v6) {
                        (true, true) => "works (IPv4 and IPv6)".to_string(),
                        (true, false) => "works (IPv4; no IPv6 answer)".to_string(),
                        (false, true) => "works (IPv6; no IPv4 answer)".to_string(),
                        (false, false) => "no answer from any relay".to_string(),
                    };
                    line(s, "UDP", udp);
                    let mut public = Vec::new();
                    if let Some(a) = &n.public_v4 {
                        let nat = match n.behind_nat_v4 {
                            Some(true) => "behind NAT",
                            Some(false) => "no NAT",
                            None => "NAT unknown",
                        };
                        let port = match n.port_preserved_v4 {
                            Some(true) => ", same port as local",
                            Some(false) => ", port changed by NAT",
                            None => "",
                        };
                        public.push(format!("{a} ({nat}{port})"));
                    }
                    if let Some(a) = &n.public_v6 {
                        public.push(a.clone());
                    }
                    if !public.is_empty() {
                        line(s, "public address", public.join(", "));
                    }
                    let mapping = match n.mapping_varies_by_dest {
                        Some(true) => "changes per destination (hard NAT)".to_string(),
                        Some(false) => "same for every destination (good for hole punching)".into(),
                        None if n.behind_nat_v4 == Some(false) => {
                            "none: no NAT on IPv4".to_string()
                        }
                        None => format!(
                            "unknown (needs UDP answers from 2 relays, got {})",
                            n.qad_relays_v4.max(n.qad_relays_v6)
                        ),
                    };
                    line(s, "NAT mapping", mapping);
                }
                None if self.transport == "relay_only" => {
                    line(s, "UDP", "not tested (--relay-only)".into())
                }
                None => line(s, "UDP", "not measured".into()),
            }
            if !l.direct_addrs.is_empty() {
                line(s, "direct addrs", l.direct_addrs.join(", "));
            }
            for d in &l.directories {
                let v = if d.ok {
                    let rec = match d.own_record {
                        Some("published") => "our record is published",
                        Some("missing") => "our record is MISSING",
                        Some("other_endpoint") => "lists ANOTHER endpoint for our identity",
                        _ => "",
                    };
                    format!("ok: {}, {rec}", opt_ms(d.lookup_ms))
                } else {
                    format!(
                        "NOT REACHABLE: {}",
                        d.error.as_deref().unwrap_or("unknown error")
                    )
                };
                line(s, "directory", format!("{}  {v}", d.url));
            }
        }

        if let Some(p) = &self.peer {
            let _ = writeln!(
                s,
                "\nPeer {}",
                self.to.as_deref().map(short_id).unwrap_or_default()
            );
            write_connect(s, &p.connect, &line);
            if let Some(l) = &p.latency {
                write_latency(s, l, &line);
            }
            if p.connect.ok {
                let path = &p.path;
                let mut v = Vec::new();
                let first = path.timeline.first();
                if let (Some(f), Some(t)) = (first, path.time_to_direct_ms) {
                    if f.path == "direct" {
                        v.push("direct from the start".to_string());
                    } else {
                        v.push(format!("{} -> direct after {}", f.path, fmt_ms(t)));
                    }
                } else {
                    v.push("never direct".to_string());
                }
                if let Some(f) = p.latency.as_ref().and_then(|l| l.direct_fraction) {
                    v.push(format!("{:.0}% of requests direct", f * 100.0));
                }
                v.push(format!(
                    "{} change{}",
                    path.changes,
                    if path.changes == 1 { "" } else { "s" }
                ));
                v.push(format!(
                    "now {}{}",
                    path.final_path,
                    path.final_remote_addr
                        .as_ref()
                        .map(|a| format!(" ({a})"))
                        .unwrap_or_default()
                ));
                line(s, "path", v.join("; "));
            }
            if let Some(c) = &p.connection_lost {
                line(
                    s,
                    "connection",
                    format!(
                        "LOST after {} (during the {} phase): {}",
                        fmt_ms(c.t_ms),
                        c.phase.replace('_', " "),
                        c.reason
                    ),
                );
            }
            if let Some(t) = &p.upload {
                line(s, "upload", transfer_line(t));
            }
            if let Some(t) = &p.download {
                line(s, "download", transfer_line(t));
            }
            if let Some(c) = &p.concurrency {
                let v = if c.requests == 0 && !c.ok {
                    format!("FAILED: {}", c.error.as_deref().unwrap_or("?"))
                } else {
                    let mut v = format!(
                        "{} streams: {} requests of {} ({} each way) in {}: {}",
                        c.streams,
                        c.requests,
                        fmt_bytes(c.request_bytes as u64),
                        fmt_bytes(c.total_bytes),
                        fmt_ms(c.duration_ms),
                        c.throughput_mbps.map_or("-".into(), fmt_mbps)
                    );
                    if c.failed > 0 {
                        let _ = write!(v, ", {} FAILED", c.failed);
                    }
                    if let Some(m) = c.loaded_rtt_ms.p50 {
                        let _ = write!(v, "; small requests meanwhile: median {}", fmt_ms(m));
                    }
                    v
                };
                line(s, "parallel", v);
            }
            if let Some(v) = loss_line(p) {
                line(s, "packet loss", v);
            }
            if let Some(i) = &p.idle {
                let v = if i.ok {
                    format!(
                        "after {} idle: connection {}, path {} -> {}, first reply after {}",
                        fmt_ms(i.idle_ms),
                        if i.survived {
                            "still open"
                        } else {
                            "was LOST and re-established"
                        },
                        i.path_before,
                        i.path_after,
                        opt_ms(i.recovery_ms)
                    )
                } else {
                    format!(
                        "after {} idle: FAILED ({} attempts): {}",
                        fmt_ms(i.idle_ms),
                        i.attempts,
                        i.error.as_deref().unwrap_or("?")
                    )
                };
                line(s, "idle", v);
            }
        }

        if let Some(rc) = &self.relay_compare {
            match &rc.skipped {
                Some(why) => {
                    let _ = writeln!(s, "\nRelay-only comparison: skipped ({why})");
                }
                None => {
                    let _ = writeln!(
                        s,
                        "\nRelay-only comparison{}",
                        rc.relay
                            .as_ref()
                            .map(|r| format!(" (via {r})"))
                            .unwrap_or_default()
                    );
                    if let Some(c) = &rc.connect {
                        write_connect(s, c, &line);
                    }
                    if let Some(l) = &rc.latency {
                        write_latency(s, l, &line);
                    }
                    if let Some(t) = &rc.upload {
                        line(s, "upload", transfer_line(t));
                    }
                    if let Some(t) = &rc.download {
                        line(s, "download", transfer_line(t));
                    }
                }
            }
        }

        let _ = writeln!(
            s,
            "\nVerdict: {} (exit code {})",
            self.status.name().to_uppercase(),
            self.exit_code
        );
        wrap(s, "  ", &self.verdict);
        let new = |f: &&Finding| !self.in_verdict.contains(&f.code);
        for f in self.issues.iter().filter(new) {
            wrap(s, "  Problem: ", &f.message);
        }
        for f in self.hints.iter().filter(new) {
            wrap(s, "  Hint: ", &f.message);
        }
        for u in &self.unknowns {
            wrap(s, "  Not measured: ", &u.reason);
        }
    }
}

/// Appends `text` after `prefix`, wrapped at 80 columns; continuation lines
/// are indented by four spaces.
fn wrap(s: &mut String, prefix: &str, text: &str) {
    const WIDTH: usize = 80;
    let mut col = prefix.len();
    s.push_str(prefix);
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 && col + 1 + word.len() > WIDTH {
            s.push_str("\n    ");
            col = 4;
        } else if i > 0 {
            s.push(' ');
            col += 1;
        }
        s.push_str(word);
        col += word.len();
    }
    s.push('\n');
}

fn write_connect(s: &mut String, c: &Connect, line: &impl Fn(&mut String, &str, String)) {
    let v = if c.ok {
        format!(
            "ok in {} (directory lookup {}, handshake {}); first path: {}{}",
            opt_ms(c.connect_ms),
            opt_ms(c.lookup_ms),
            opt_ms(c.handshake_ms),
            c.path,
            c.remote_addr
                .as_ref()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "FAILED after {}: {}",
            opt_ms(c.connect_ms),
            c.error.as_deref().unwrap_or("?")
        )
    };
    line(s, "connect", v);
}

fn write_latency(s: &mut String, l: &Latency, line: &impl Fn(&mut String, &str, String)) {
    let r = &l.rtt_ms;
    let mut v = format!(
        "{} requests in {}, {} failed",
        l.sent,
        fmt_ms(l.duration_ms),
        l.failed
    );
    if r.p50.is_some() {
        v += &format!(
            "; round trip min {} / median {} / p95 {} / max {}",
            opt_ms(r.min),
            opt_ms(r.p50),
            opt_ms(r.p95),
            opt_ms(r.max)
        );
    }
    if let Some(j) = l.jitter_ms {
        v += &format!("; jitter {}", fmt_ms(j));
    }
    line(s, "latency", v);
}

/// The QUIC counters of the upload, download and concurrency phases added
/// up; `None` if none of them has counters.
fn transfer_quic(p: &Peer) -> Option<QuicStats> {
    let phases = [
        p.upload.as_ref().and_then(|t| t.quic.as_ref()),
        p.download.as_ref().and_then(|t| t.quic.as_ref()),
        p.concurrency.as_ref().and_then(|c| c.quic.as_ref()),
    ];
    let mut sum: Option<QuicStats> = None;
    for q in phases.into_iter().flatten() {
        let s = sum.get_or_insert_with(QuicStats::default);
        s.sent_packets += q.sent_packets;
        s.lost_packets += q.lost_packets;
        s.sent_bytes += q.sent_bytes;
        s.received_bytes += q.received_bytes;
    }
    if let Some(s) = &mut sum {
        s.lost_pct = (s.sent_packets > 0)
            .then(|| crate::out::round3(s.lost_packets as f64 * 100.0 / s.sent_packets as f64));
    }
    sum
}

/// Packet loss of the latency phase, and of the transfers for information.
fn loss_line(p: &Peer) -> Option<String> {
    let pct = |q: &QuicStats| q.lost_pct.map(|v| (v, q.sent_packets));
    let mut v = Vec::new();
    if let Some((l, n)) = p
        .latency
        .as_ref()
        .and_then(|l| l.quic.as_ref())
        .and_then(pct)
    {
        v.push(format!("{l:.2}% of {n} packets while measuring latency"));
    }
    if let Some((l, n)) = transfer_quic(p).as_ref().and_then(pct) {
        v.push(format!(
            "{l:.2}% of {n} in the transfers (congestion control loses some there by design)"
        ));
    }
    if v.is_empty() {
        let (l, n) = p.quic.as_ref().and_then(pct)?;
        v.push(format!("{l:.2}% of {n} packets"));
    }
    Some(format!("{}; this node's packets only", v.join("; ")))
}

fn transfer_line(t: &TransferReport) -> String {
    let cap = if t.relay_capped {
        " (size capped for the relay)"
    } else {
        ""
    };
    match (t.ok, t.bytes, t.duration_ms, t.throughput_mbps) {
        (true, Some(b), Some(d), Some(m)) => format!(
            "{} in {}: {} ({}{}){cap}",
            fmt_bytes(b),
            fmt_ms(d),
            fmt_mbps(m),
            t.path.unwrap_or("unknown"),
            if t.limited_by == Some("time") {
                ", stopped at the time limit"
            } else {
                ""
            }
        ),
        _ => format!(
            "FAILED ({}){cap}: {}",
            fmt_bytes(t.requested_bytes),
            t.error.as_deref().unwrap_or("?")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping() {
        let mut s = String::new();
        let text = "word ".repeat(30);
        wrap(&mut s, "  Hint: ", &text);
        let lines: Vec<_> = s.lines().collect();
        assert!(lines.len() > 1, "{s}");
        assert!(lines.iter().all(|l| l.len() <= 80), "{s}");
        assert!(lines[0].starts_with("  Hint: word"));
        assert!(lines[1].starts_with("    word"));
        assert_eq!(s.matches("word").count(), 30);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_ms(4.567), "4.6 ms");
        assert_eq!(fmt_ms(350.2), "350 ms");
        assert_eq!(fmt_ms(1234.0), "1.2 s");
        assert_eq!(fmt_ms(61_000.0), "61 s");
        assert_eq!(fmt_bytes(64), "64 B");
        assert_eq!(fmt_bytes(262_144), "262 kB");
        assert_eq!(fmt_bytes(10_000_000), "10.0 MB");
        assert_eq!(fmt_mbps(38.12), "38.1 Mbps");
        assert_eq!(fmt_mbps(250.4), "250 Mbps");
        assert_eq!(fmt_mbps(0.85), "850 kbps");
        assert_eq!(
            short_id("0x00a329c0648769a73afac7f9381e08fb43dbea72"),
            "0x00a3...ea72"
        );
    }
}
