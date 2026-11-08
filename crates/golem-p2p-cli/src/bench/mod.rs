//! `bench`: one command that checks the local network (part A) and, with
//! `--to`, benchmarks the connection to a node running `listen` (part B),
//! then explains the result in plain words. Traffic and time are bounded.
//!
//! Phases of part B, in order: connect, latency, upload, download,
//! concurrency, relay-only comparison (`--compare-relay`), idle (`--idle`).
//! Each phase is bounded by size and time; transfers over a relay are capped
//! at `--relay-transfer-size` to spare shared relays.

mod local;
mod report;
mod verdict;

use std::{
    collections::HashMap,
    process::ExitCode,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::bail;
use clap::{Args, ValueEnum};
use golem_p2p::{
    Connection, Discovery, DiscoveryError, Endpoint, Identities, InMemorySigner, MultiDiscovery,
    NodeId, PathKind, TransportMode, VerifiedRecord, signer::BoxFuture,
};
use serde::Serialize;

use self::report::{
    Concurrency, Connect, ConnectionLost, Idle, Latency, LatencyLoss, PathPoint, PathReport, Peer,
    QuicStats, RelayCompare, Report, Restart, Settings, Status, TransferReport,
};
use crate::{
    Node, NodeArgs, listen,
    observe::{PathLog, SelectedPath, connection_to, path_to},
    out::{Out, anyhow_kind, error_kind, ms, path_name, round3},
    parse,
    probe::{self, Pinger, Stats},
};

/// Size of each request of the concurrency phase (smaller on slow links,
/// see [`concurrency_request_size`]).
const CONCURRENCY_REQUEST: usize = 256 * 1024;

/// The smallest request of the concurrency phase on a slow link.
const MIN_CONCURRENCY_REQUEST: usize = 4 * 1024;

/// On a slow link the concurrency phase's requests are made smaller, so that
/// every stream can finish about this many within `--transfer-time`.
const REQUESTS_PER_STREAM: f64 = 4.0;

/// If none of the concurrency phase's requests has finished by half of
/// `--transfer-time`, it starts over with requests this many times smaller
/// (see [`Workers`]).
const RESTART_SHRINK: usize = 16;

/// The relay-only comparison measures latency for at most this long.
const RELAY_LATENCY_MAX: Duration = Duration::from_secs(5);

/// Requests after the idle time before giving up.
const IDLE_ATTEMPTS: u64 = 3;

/// The latency phase counts packet loss only once the selected path has been
/// unchanged this long, and at least [`SETTLE_RTTS`] times the shortest round
/// trip seen (the shortest: a request slowed down by loss must not restart
/// the count): setting up a path loses packets by design. iroh's hole punching sends probes that
/// the peer's NAT drops until the hole is open, and they take their packet
/// numbers from the relayed path, so QUIC also declares relayed packets lost
/// that a probe's acknowledgement overtook (campaign 2, debug/latency-loss:
/// 1-6 of ~120 packets on clean links, all within 75 ms of the switch).
const PATH_SETTLE: Duration = Duration::from_secs(1);

/// See [`PATH_SETTLE`].
const SETTLE_RTTS: u32 = 3;

#[derive(Args)]
pub struct BenchArgs {
    #[command(flatten)]
    node: NodeArgs,
    /// Node ID of a node running `listen` to benchmark. Without it, only
    /// the local network check runs.
    #[arg(long)]
    to: Option<NodeId>,
    /// Phases to skip, comma separated.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "PHASE,...")]
    skip: Vec<Phase>,
    /// Timeout for connecting and for each request.
    #[arg(long, default_value = "10s", value_parser = parse::duration)]
    timeout: Duration,
    /// How long to measure latency, with one request per `--interval`.
    /// Packet loss on the path is counted meanwhile (once the path has
    /// settled after hole punching).
    #[arg(long, default_value = "12s", value_parser = parse::duration)]
    latency_duration: Duration,
    /// Time between the starts of two latency requests.
    #[arg(long, default_value = "200ms", value_parser = parse::duration)]
    interval: Duration,
    /// Size of each latency request (at most 1 MiB).
    #[arg(long, default_value = "64", value_parser = parse::bytes)]
    payload_size: u64,
    /// Bytes to upload, and to download, e.g. `10M` (decimal) or `8MiB`.
    #[arg(long, default_value = "10M", value_parser = parse::bytes)]
    transfer_size: u64,
    /// Longest time for the upload, the download and the concurrency phase
    /// each; a transfer stops there and reports what it moved.
    #[arg(long, default_value = "8s", value_parser = parse::duration)]
    transfer_time: Duration,
    /// Size cap for every upload, download and concurrency phase that runs
    /// through a relay (also with `--compare-relay`), to spare shared relays.
    #[arg(long, default_value = "4M", value_parser = parse::bytes)]
    relay_transfer_size: u64,
    /// Parallel request streams in the concurrency phase (at most 64).
    #[arg(long, default_value_t = 8)]
    streams: usize,
    /// Total request bytes of the concurrency phase, in requests of 256 KiB;
    /// every request is echoed back.
    #[arg(long, default_value = "8M", value_parser = parse::bytes)]
    concurrency_size: u64,
    /// Also measure latency and throughput over a relay-only connection (a
    /// second, temporary endpoint without UDP): what you get if hole punching
    /// fails.
    #[arg(long, requires = "to")]
    compare_relay: bool,
    /// At the end, wait this long without traffic, then check whether the
    /// connection survived and how long the next request takes, e.g. `60s`.
    #[arg(long, requires = "to", value_parser = parse::duration)]
    idle: Option<Duration>,
}

/// A phase that `--skip` can leave out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Phase {
    /// Local network check (part A).
    Local,
    /// Latency series.
    Latency,
    /// Upload throughput.
    Upload,
    /// Download throughput.
    Download,
    /// Parallel requests.
    Concurrency,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Local => "local",
            Phase::Latency => "latency",
            Phase::Upload => "upload",
            Phase::Download => "download",
            Phase::Concurrency => "concurrency",
        }
    }
}

impl BenchArgs {
    fn runs(&self, phase: Phase) -> bool {
        !self.skip.contains(&phase)
    }

    fn validate(&self) -> anyhow::Result<usize> {
        if self.interval.is_zero() {
            bail!("--interval must be positive");
        }
        if self.streams > 64 {
            bail!("--streams must be at most 64");
        }
        if self.transfer_time.is_zero() {
            bail!("--transfer-time must be positive");
        }
        usize::try_from(self.payload_size)
            .ok()
            .filter(|s| *s <= listen::MAX_REQUEST)
            .ok_or_else(|| {
                anyhow::anyhow!("--payload-size must be at most {}", listen::MAX_REQUEST)
            })
    }

    fn settings(&self, payload: usize) -> Settings {
        Settings {
            timeout_ms: ms(self.timeout),
            latency_duration_ms: ms(self.latency_duration),
            interval_ms: ms(self.interval),
            payload_bytes: payload,
            transfer_bytes: self.transfer_size,
            transfer_time_ms: ms(self.transfer_time),
            relay_transfer_bytes: self.relay_transfer_size,
            streams: self.streams,
            concurrency_bytes: self.concurrency_size,
            concurrency_request_bytes: self.request_size(),
            compare_relay: self.compare_relay,
            idle_ms: self.idle.map(ms),
            skip: self.skip.iter().map(|p| p.name()).collect(),
        }
    }

    fn request_size(&self) -> usize {
        CONCURRENCY_REQUEST.min(self.concurrency_size.max(1) as usize)
    }
}

/// Discovery that times lookups and optionally does not publish.
#[derive(Debug)]
struct BenchDiscovery {
    inner: MultiDiscovery,
    publish: bool,
    lookups: Mutex<HashMap<NodeId, Duration>>,
}

impl BenchDiscovery {
    fn new(inner: MultiDiscovery, publish: bool) -> Arc<Self> {
        Arc::new(Self {
            inner,
            publish,
            lookups: Mutex::new(HashMap::new()),
        })
    }

    /// Duration of the last lookup of `id`.
    fn lookup_time(&self, id: NodeId) -> Option<Duration> {
        self.lookups.lock().expect("poisoned").get(&id).copied()
    }
}

impl Discovery for BenchDiscovery {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        if self.publish {
            self.inner.publish(record)
        } else {
            Box::pin(async { Ok(()) })
        }
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        Box::pin(async move {
            let start = Instant::now();
            let r = self.inner.lookup(id).await;
            self.lookups
                .lock()
                .expect("poisoned")
                .insert(id, start.elapsed());
            r
        })
    }
}

/// Prints a progress line on stderr (human mode only).
struct Progress(bool);

impl Progress {
    fn say(&self, msg: impl AsRef<str>) {
        if self.0 {
            eprintln!("{}", msg.as_ref());
        }
    }
}

/// Emits a `bench_phase` event.
fn phase_event(out: &Out, phase: &str, ok: bool, took: Duration, result: &impl Serialize) {
    out.event("bench_phase")
        .field("phase", phase)
        .field("ok", ok)
        .field("duration_ms", ms(took))
        .field_ser("result", result)
        .emit();
}

/// Runs `fut` unless a stop signal arrives first (then `None`).
async fn unless_stopped<T, F: Future<Output = ()>>(
    fut: impl Future<Output = T>,
    stop: &impl Fn() -> F,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = stop() => None,
        r = fut => Some(r),
    }
}

pub async fn run(args: BenchArgs, out: &Out) -> anyhow::Result<ExitCode> {
    let payload_size = args.validate()?;
    let started = Instant::now();
    let progress = Progress(!out.json());
    let path_log = args.to.map(|to| Arc::new(PathLog::new(to)));

    let (signer, ids) = crate::identities(&args.node)?;
    let discovery = BenchDiscovery::new(crate::directories(&args.node)?, true);
    progress.say("Starting a node...");
    let bind_start = Instant::now();
    let ep = crate::builder(
        &args.node,
        signer,
        ids,
        discovery.clone(),
        Some(args.timeout),
    )?
    .bind()
    .await?;
    let node = Node::bound(ep, bind_start.elapsed(), &args.node, out, path_log.clone());
    let ep = &node.ep;
    let stop = crate::stop_signal();

    let transport = match args.node.mode() {
        TransportMode::DirectAndRelay => "direct_and_relay",
        TransportMode::RelayOnly => "relay_only",
        TransportMode::DirectOnly => "direct_only",
    };
    let mut report = Report {
        status: Status::Healthy,
        exit_code: 0,
        verdict: String::new(),
        issues: Vec::new(),
        hints: Vec::new(),
        unknowns: Vec::new(),
        node_id: ep.node_id().to_string(),
        endpoint_id: ep.endpoint_id().to_string(),
        temporary_identity: args.node.key.is_none(),
        transport,
        to: args.to.map(|t| t.to_string()),
        duration_ms: 0.0,
        interrupted: false,
        settings: args.settings(payload_size),
        local: None,
        peer: None,
        relay_compare: None,
        in_verdict: Vec::new(),
    };

    // Part A: the local network.
    if args.runs(Phase::Local) {
        progress.say("Checking the local network (relays, directories, UDP, NAT)...");
        let t = Instant::now();
        match unless_stopped(local::check(ep, &args.node), &stop).await {
            Some(l) => {
                let ok = l.relays.iter().all(|r| r.ok) && l.directories.iter().all(|d| d.ok);
                phase_event(out, "local", ok, t.elapsed(), &l);
                report.local = Some(l);
            }
            None => report.interrupted = true,
        }
    }

    // Part B: the peer.
    if let (Some(to), Some(log), false) = (args.to, &path_log, report.interrupted) {
        let bench = PeerBench {
            args: &args,
            ep,
            to,
            out,
            progress: &progress,
            payload_size,
            phase: Arc::new(Mutex::new("connect")),
        };
        let (peer, relay_compare, interrupted) = bench.run(log, &discovery, &stop).await;
        report.peer = Some(peer);
        report.relay_compare = relay_compare;
        report.interrupted |= interrupted;
    }

    report.duration_ms = ms(started.elapsed());
    verdict::analyze(&mut report);
    if out.json() {
        out.event("bench_report").fields_ser(&report).emit_last();
    } else {
        report.print_human();
    }
    ep.close().await;
    Ok(ExitCode::from(report.exit_code))
}

/// Part B against one peer.
struct PeerBench<'a> {
    args: &'a BenchArgs,
    ep: &'a Endpoint,
    to: NodeId,
    out: &'a Out,
    progress: &'a Progress,
    payload_size: usize,
    /// The phase that is running, to tell when a connection was lost.
    phase: Arc<Mutex<&'static str>>,
}

impl PeerBench<'_> {
    fn enter(&self, phase: &'static str) {
        *self.phase.lock().expect("poisoned") = phase;
    }

    /// Runs the phases of part B. Returns the peer section, the relay-only
    /// comparison, and whether a signal ended the run early.
    async fn run<F: Future<Output = ()>>(
        &self,
        log: &Arc<PathLog>,
        discovery: &BenchDiscovery,
        stop: &impl Fn() -> F,
    ) -> (Peer, Option<RelayCompare>, bool) {
        let args = self.args;
        let (ep, to) = (self.ep, self.to);
        let t0 = Instant::now();
        self.progress.say(format!(
            "Connecting to {to} (timeout {})...",
            fmt_dur(args.timeout)
        ));
        let connect = unless_stopped(connect(ep, to, discovery), stop).await;
        let mut peer = Peer {
            connect: Connect::default(),
            latency: None,
            upload: None,
            download: None,
            concurrency: None,
            idle: None,
            path: PathReport::default(),
            quic: None,
            connection_lost: None,
        };
        let Some((connect, conn)) = connect else {
            peer.connect = Connect {
                path: "unknown",
                error_kind: Some("interrupted"),
                error: Some("interrupted".into()),
                ..Default::default()
            };
            return (peer, None, true);
        };
        phase_event(self.out, "connect", connect.ok, t0.elapsed(), &connect);
        peer.connect = connect;
        let mut relay_compare = None;
        let mut interrupted = false;
        if let Some(conn) = conn {
            let base = QuicSnapshot::of(&conn);
            let lost = Arc::new(OnceLock::new());
            let watch = tokio::spawn({
                let (conn, phase, lost) = (conn.clone(), self.phase.clone(), lost.clone());
                async move {
                    conn.closed().await;
                    let phase = *phase.lock().expect("poisoned");
                    let _ = lost.set((Instant::now(), phase));
                }
            });
            interrupted = self.phases(&mut peer, &mut relay_compare, log, stop).await;
            watch.abort();
            // QUIC counters of the connection: since connecting if it is
            // still the same one, else of the current one.
            peer.quic = QuicSnapshot::to(ep, to)
                .map(|now| now.since((now.conn == base.conn).then_some(&base)));
            if let Some(reason) = conn.iroh_connection().close_reason() {
                let (at, phase) = lost.get().copied().unwrap_or((Instant::now(), "unknown"));
                peer.connection_lost = Some(ConnectionLost {
                    t_ms: ms(at.saturating_duration_since(t0)),
                    phase,
                    cause: close_cause(&reason),
                    reason: reason.to_string(),
                });
            }
        }
        peer.path = path_report(ep, to, log, t0);
        (peer, relay_compare, interrupted)
    }

    /// The phases after connecting. Returns `true` if interrupted.
    async fn phases<F: Future<Output = ()>>(
        &self,
        peer: &mut Peer,
        relay_compare: &mut Option<RelayCompare>,
        log: &Arc<PathLog>,
        stop: &impl Fn() -> F,
    ) -> bool {
        let args = self.args;
        let (ep, to, out) = (self.ep, self.to, self.out);

        if args.runs(Phase::Latency) {
            self.enter("latency");
            self.progress.say(format!(
                "Measuring latency for {} (one request every {})...",
                fmt_dur(args.latency_duration),
                fmt_dur(args.interval)
            ));
            let t = Instant::now();
            let before = QuicSnapshot::to(ep, to);
            let mut loss = LossCount::new(ep, to, t);
            let mut pinger = Pinger::new(ep, to, out, log.clone()).with_phase("latency");
            let interrupted = probe::ping_series(
                &mut pinger,
                args.latency_duration,
                args.interval,
                self.payload_size,
                args.timeout,
                stop,
                |reply, _| loss.on_reply(ep, to, log, reply),
            )
            .await;
            let mut l = latency(&pinger, self.payload_size);
            l.quic = phase_quic(ep, to, before.as_ref());
            l.loss = loss.finish(ep, to, log);
            phase_event(out, "latency", l.failed == 0, t.elapsed(), &l);
            peer.latency = Some(l);
            if interrupted {
                return true;
            }
        }

        for (phase, is_upload) in [(Phase::Upload, true), (Phase::Download, false)] {
            if !args.runs(phase) {
                continue;
            }
            self.enter(phase.name());
            let t = Instant::now();
            let r = self.transfer(ep, is_upload, args.transfer_size);
            let Some(r) = unless_stopped(r, stop).await else {
                return true;
            };
            phase_event(out, phase.name(), r.ok, t.elapsed(), &r);
            if is_upload {
                peer.upload = Some(r);
            } else {
                peer.download = Some(r);
            }
        }

        if args.runs(Phase::Concurrency) && args.streams > 0 && args.concurrency_size > 0 {
            self.enter("concurrency");
            let t = Instant::now();
            // The slower direction of the transfers just measured, each by
            // what it sustained.
            let link_mbps = [&peer.upload, &peer.download]
                .into_iter()
                .flatten()
                .filter_map(sustained_mbps)
                .reduce(f64::min);
            let Some(c) = unless_stopped(self.concurrency(link_mbps), stop).await else {
                return true;
            };
            phase_event(out, "concurrency", c.ok, t.elapsed(), &c);
            peer.concurrency = Some(c);
        }

        if args.compare_relay {
            self.enter("relay_compare");
            let t = Instant::now();
            let Some(rc) = unless_stopped(self.relay_compare(stop), stop).await else {
                return true;
            };
            phase_event(out, "relay_compare", rc.ok, t.elapsed(), &rc);
            *relay_compare = Some(rc);
        }

        if let Some(d) = args.idle {
            self.enter("idle");
            let t = Instant::now();
            let Some(i) = unless_stopped(self.idle(d, log), stop).await else {
                return true;
            };
            phase_event(out, "idle", i.ok, t.elapsed(), &i);
            peer.idle = Some(i);
        }
        false
    }

    /// One upload or download, bounded by size and `--transfer-time`; over a
    /// relay the size is capped by `--relay-transfer-size`.
    async fn transfer(&self, ep: &Endpoint, is_upload: bool, size: u64) -> TransferReport {
        let args = self.args;
        let relayed = path_to(ep, self.to) == PathKind::Relay;
        let capped = relayed && size > args.relay_transfer_size;
        let size = if capped {
            args.relay_transfer_size
        } else {
            size
        };
        self.progress.say(format!(
            "{} up to {} (at most {})...",
            if is_upload {
                "Uploading"
            } else {
                "Downloading"
            },
            report::fmt_bytes(size),
            fmt_dur(args.transfer_time)
        ));
        // A safety net: the transfers end at the time limit themselves (an
        // upload then waits for the listener's count, see `count_wait`).
        let limit = args.transfer_time + probe::COUNT_WAIT_MAX + Duration::from_secs(1);
        let before = QuicSnapshot::to(ep, self.to);
        let run = async {
            if is_upload {
                probe::upload(ep, self.to, size, Some(args.transfer_time)).await
            } else {
                probe::download(ep, self.to, size, Some(args.transfer_time)).await
            }
        };
        let result = tokio::time::timeout(limit, run)
            .await
            .unwrap_or_else(|_| Err(golem_p2p::Error::Timeout.into()));
        let mut r = transfer_report(size, capped, result);
        r.quic = phase_quic(ep, self.to, before.as_ref());
        r
    }

    /// `--streams` parallel workers sending echo requests of 256 KiB (less
    /// on a link measured at under `link_mbps`, see
    /// [`concurrency_request_size`]) until `--concurrency-size` bytes or
    /// `--transfer-time` (starting over with smaller requests if none has
    /// finished by half of it, see [`Workers`]), while small requests measure
    /// the round-trip time under load.
    async fn concurrency(&self, link_mbps: Option<f64>) -> Concurrency {
        let args = self.args;
        let (ep, to) = (self.ep, self.to);
        let relayed = path_to(ep, to) == PathKind::Relay;
        let budget = if relayed {
            args.concurrency_size.min(args.relay_transfer_size)
        } else {
            args.concurrency_size
        };
        let size = concurrency_request_size(
            args.request_size(),
            link_mbps,
            args.streams,
            args.transfer_time,
        );
        self.progress.say(format!(
            "Sending {} in parallel over {} streams (at most {})...",
            report::fmt_bytes(budget),
            args.streams,
            fmt_dur(args.transfer_time)
        ));
        let before = QuicSnapshot::to(ep, to);
        let workers = Workers {
            streams: args.streams,
            budget,
            size,
            time: args.transfer_time,
        };
        let request = |payload: Vec<u8>| async move {
            let reply = ep.request(to, &payload).await?;
            if !reply.ends_with(&payload) {
                bail!("unexpected reply");
            }
            Ok(())
        };
        // The phase ends when the last worker does: its duration is taken
        // then (by `Workers::run`), not when the small requests below notice
        // (campaign 3: on fast paths every duration snapped to their 200 ms
        // ticks).
        let workers = workers.run(request);
        // Small requests every --interval while the workers run; the one in
        // flight when they are done is dropped.
        let mut small = Vec::new();
        let small_requests = async {
            let mut tick = tokio::time::interval(args.interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for n in 1.. {
                tick.tick().await;
                let payload = probe::payload(n, self.payload_size);
                let t = Instant::now();
                if let Ok(reply) = ep.request(to, &payload).await
                    && reply.ends_with(&payload)
                {
                    small.push(t.elapsed());
                }
            }
        };
        let r = tokio::select! {
            r = workers => r,
            () = small_requests => unreachable!("the small requests go on until the workers end"),
        };
        let total = r.rtts.len() as u64 * r.size as u64;
        Concurrency {
            ok: r.failed == 0,
            streams: args.streams,
            request_bytes: r.size,
            sized_for_mbps: link_mbps.filter(|_| size < args.request_size()),
            restart: r.restart.map(|(at, cut_off)| Restart {
                at_ms: ms(at),
                cut_off,
                request_bytes: size,
            }),
            requests: r.rtts.len() as u64,
            failed: r.failed,
            unfinished: r.unfinished,
            limited_by: if r.by_time { "time" } else { "size" },
            total_bytes: total,
            duration_ms: ms(r.elapsed),
            throughput_mbps: (!r.rtts.is_empty()).then(|| round3(probe::mbps(total, r.elapsed))),
            rtt_ms: Stats::of(&r.rtts),
            loaded_rtt_ms: Stats::of(&small),
            loaded_samples: small.len(),
            error_kind: r.error.as_ref().map(|e| anyhow_kind(e, "request")),
            error: r.error.map(|e| format!("{e:#}")),
            quic: phase_quic(ep, to, before.as_ref()),
        }
    }

    /// Latency and throughput over a second, relay-only endpoint with a
    /// temporary identity that is not published.
    async fn relay_compare<F: Future<Output = ()>>(&self, stop: &impl Fn() -> F) -> RelayCompare {
        let args = self.args;
        let skipped = |why: &str| RelayCompare {
            skipped: Some(why.into()),
            ..Default::default()
        };
        if args.node.relay.is_empty() {
            return skipped("no relay configured");
        }
        if args.node.relay_only {
            return skipped("the main connection is relay-only already (--relay-only)");
        }
        self.progress
            .say("Comparing with a relay-only connection (second, temporary endpoint)...");
        let ep2 = match self.relay_endpoint().await {
            Ok(e) => e,
            Err(e) => {
                return RelayCompare {
                    connect: Some(Connect {
                        path: "unknown",
                        error_kind: Some(anyhow_kind(&e, "bind")),
                        error: Some(format!("{e:#}")),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
            }
        };
        let (ep, discovery) = &ep2;
        let mut rc = RelayCompare::default();
        let (c, conn) = connect(ep, self.to, discovery).await;
        rc.relay = c.remote_addr.clone().filter(|_| c.path == "relay");
        let connected = c.ok;
        rc.connect = Some(c);
        if let Some(conn) = conn {
            let base = QuicSnapshot::of(&conn);
            let log = Arc::new(PathLog::new(self.to));
            let mut loss = LossCount::new(ep, self.to, Instant::now());
            let mut pinger =
                Pinger::new(ep, self.to, self.out, log.clone()).with_phase("relay_latency");
            probe::ping_series(
                &mut pinger,
                args.latency_duration.min(RELAY_LATENCY_MAX),
                args.interval,
                self.payload_size,
                args.timeout,
                stop,
                |reply, _| loss.on_reply(ep, self.to, &log, reply),
            )
            .await;
            let mut l = latency(&pinger, self.payload_size);
            l.quic = phase_quic(ep, self.to, Some(&base));
            l.loss = loss.finish(ep, self.to, &log);
            let size = args.transfer_size.min(args.relay_transfer_size);
            let capped = size < args.transfer_size;
            let up = if args.runs(Phase::Upload) {
                Some(self.transfer_on(ep, true, size, capped).await)
            } else {
                None
            };
            let down = if args.runs(Phase::Download) {
                Some(self.transfer_on(ep, false, size, capped).await)
            } else {
                None
            };
            // Reachable: something went through. Everything else that
            // failed (slow transfers, some requests) makes it not `ok`.
            rc.reachable =
                connected && (l.ok > 0 || [&up, &down].into_iter().flatten().any(|t| t.ok));
            rc.ok = rc.reachable
                && l.failed == 0
                && l.ok > 0
                && up.as_ref().is_none_or(|t| t.ok)
                && down.as_ref().is_none_or(|t| t.ok);
            rc.latency = Some(l);
            rc.upload = up;
            rc.download = down;
            rc.quic = QuicSnapshot::to(ep, self.to)
                .map(|now| now.since((now.conn == base.conn).then_some(&base)));
            if rc.relay.is_none() {
                rc.relay = connection_to(ep, self.to)
                    .and_then(|c| SelectedPath::of(&c).remote)
                    .filter(|_| path_to(ep, self.to) == PathKind::Relay);
            }
        }
        ep.close().await;
        rc
    }

    /// An upload or download on the relay-only endpoint (already capped).
    async fn transfer_on(
        &self,
        ep: &Endpoint,
        is_upload: bool,
        size: u64,
        capped: bool,
    ) -> TransferReport {
        let mut r = self.transfer(ep, is_upload, size).await;
        r.relay_capped |= capped;
        r
    }

    async fn relay_endpoint(&self) -> anyhow::Result<(Endpoint, Arc<BenchDiscovery>)> {
        let signer = InMemorySigner::new();
        let id = signer.generate();
        let discovery = BenchDiscovery::new(crate::directories(&self.args.node)?, false);
        let ep = crate::builder(
            &self.args.node,
            signer,
            Identities::new(id),
            discovery.clone(),
            Some(self.args.timeout),
        )?
        .transport_mode(TransportMode::RelayOnly)
        .bind()
        .await?;
        Ok((ep, discovery))
    }

    /// Waits `idle` without traffic, then sends requests until one succeeds
    /// (at most 3).
    async fn idle(&self, idle: Duration, log: &Arc<PathLog>) -> Idle {
        let (ep, to) = (self.ep, self.to);
        self.progress
            .say(format!("Waiting {} without traffic...", fmt_dur(idle)));
        let before = connection_to(ep, to);
        let path_before = path_name(path_to(ep, to));
        tokio::time::sleep(idle).await;
        let after = connection_to(ep, to);
        let survived =
            matches!((&before, &after), (Some(b), Some(a)) if b.stable_id() == a.stable_id());
        let mut pinger = Pinger::new(ep, to, self.out, log.clone()).with_phase("idle");
        let start = Instant::now();
        let mut result = Idle {
            idle_ms: ms(idle),
            survived,
            path_before,
            path_after: "unknown",
            ..Default::default()
        };
        for _ in 0..IDLE_ATTEMPTS {
            let payload = probe::payload(pinger.sent() + 1, self.payload_size);
            let r = pinger
                .ping(
                    &payload,
                    std::future::pending(),
                    |reply| reply.ends_with(&payload),
                    false,
                )
                .await;
            result.attempts += 1;
            match r.result {
                Ok(_) => {
                    result.ok = true;
                    result.recovery_ms = Some(ms(start.elapsed()));
                    result.rtt_ms = Some(ms(r.rtt));
                    break;
                }
                Err(e) => result.error = Some(format!("{e:#}")),
            }
        }
        if result.ok {
            result.error = None;
        }
        result.path_after = path_name(path_to(ep, to));
        result
    }
}

/// Where the latency phase's loss count stands, see [`PATH_SETTLE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LossWindow {
    /// The selected path has not changed since `settle` before the count
    /// started: count on.
    Counting,
    /// The path changed less than `settle` before the count started, or
    /// after that, and has been unchanged for `settle` now: start again.
    Restart,
    /// The path changed less than `settle` ago.
    Unsettled,
}

impl LossWindow {
    /// The state of a count started at `from`, given the last change of the
    /// selected path.
    fn at(from: Instant, last_change: Option<Instant>, settle: Duration, now: Instant) -> Self {
        match last_change {
            Some(c) if c + settle > from && now >= c + settle => Self::Restart,
            Some(c) if c + settle > from => Self::Unsettled,
            _ => Self::Counting,
        }
    }
}

/// Counts the latency phase's packet loss from when the selected path has
/// settled (see [`PATH_SETTLE`]).
struct LossCount {
    /// The start of the phase.
    start: Instant,
    /// The start of the count, and the counters then.
    from: Instant,
    base: Option<QuicSnapshot>,
    /// The shortest round trip so far.
    min_rtt: Option<Duration>,
}

impl LossCount {
    fn new(ep: &Endpoint, to: NodeId, start: Instant) -> Self {
        Self {
            start,
            from: start,
            base: QuicSnapshot::to(ep, to),
            min_rtt: None,
        }
    }

    /// How long the path must be unchanged, see [`PATH_SETTLE`].
    fn settle(&self) -> Duration {
        let rtts = self.min_rtt.map_or(Duration::ZERO, |r| r * SETTLE_RTTS);
        PATH_SETTLE.max(rtts.min(PATH_SETTLE * 10))
    }

    /// After every request: starts the count again once a path change has
    /// settled.
    fn on_reply(&mut self, ep: &Endpoint, to: NodeId, log: &PathLog, reply: &probe::Reply) {
        if reply.result.is_ok() {
            self.min_rtt = Some(self.min_rtt.map_or(reply.rtt, |m| m.min(reply.rtt)));
        }
        let now = Instant::now();
        if LossWindow::at(self.from, log.last_change(), self.settle(), now) == LossWindow::Restart {
            self.from = now;
            self.base = QuicSnapshot::to(ep, to);
        }
    }

    /// The loss since the count started; `None` if the path had not settled
    /// by the end of the phase, or the connection changed.
    fn finish(self, ep: &Endpoint, to: NodeId, log: &PathLog) -> Option<LatencyLoss> {
        let now = Instant::now();
        if LossWindow::at(self.from, log.last_change(), self.settle(), now) != LossWindow::Counting
        {
            return None;
        }
        let q = phase_quic(ep, to, self.base.as_ref())?;
        Some(LatencyLoss {
            from_ms: ms(self.from.saturating_duration_since(self.start)),
            sent_packets: q.sent_packets,
            lost_packets: q.lost_packets,
            lost_pct: q.lost_pct,
        })
    }
}

/// The concurrency phase's workers: `streams` of them send echo requests of
/// `size` one after another, until they have started `budget` bytes of
/// requests or for `time`; a request still running then is cut off (that is
/// not a failure).
///
/// If none of the requests has finished by half of `time`, the size was too
/// large for the link (sized for transfers that overstated it, or the link
/// slowed down): the workers cut off their requests and start over with
/// requests [`RESTART_SHRINK`] times smaller (whole KiB, at least
/// [`MIN_CONCURRENCY_REQUEST`]), and only those are counted. From 256 KiB
/// that is 16 KiB, of which 8 streams finish a round trip in the remaining
/// 4 s (with the defaults) down to about 0.5 Mbps.
struct Workers {
    streams: usize,
    budget: u64,
    size: usize,
    time: Duration,
}

/// What the concurrency phase's workers did. After a restart, all but
/// `restart` are of the requests since.
#[derive(Default)]
struct Requests {
    /// Size of the counted requests.
    size: usize,
    /// Round-trip times of the correct replies.
    rtts: Vec<Duration>,
    failed: u64,
    /// Cut off by the time limit.
    unfinished: u64,
    /// The first error.
    error: Option<anyhow::Error>,
    /// Ended by the time limit (else by the budget).
    by_time: bool,
    /// From the start, or the restart, until the last worker ended.
    elapsed: Duration,
    /// When the workers started over (since the start), and how many requests
    /// they cut off then.
    restart: Option<(Duration, u64)>,
}

impl Workers {
    /// The size of the requests after a restart.
    fn restart_size(&self) -> usize {
        (self.size / RESTART_SHRINK / 1024 * 1024)
            .max(MIN_CONCURRENCY_REQUEST)
            .min(self.size)
    }

    /// Runs the workers. `request` sends one echo request and checks the
    /// reply.
    async fn run<F, Fut>(&self, request: F) -> Requests
    where
        F: Fn(Vec<u8>) -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let start = Instant::now();
        let deadline = tokio::time::Instant::from_std(start + self.time);
        let restart_at = tokio::time::Instant::from_std(start + self.time / 2);
        let small = self.restart_size();
        let reserved = AtomicU64::new(0);
        let seq = AtomicU64::new(0);
        let by_time = AtomicBool::new(false);
        let cut_off = AtomicU64::new(0);
        // A request has finished (replied or failed, not cut off).
        let finished = AtomicBool::new(false);
        // Taken once, at `restart_at`: when the workers started over, or
        // `None` if they go on.
        let restart = OnceLock::<Option<Instant>>::new();
        let restarted = || matches!(restart.get(), Some(Some(_)));
        let results = Mutex::new(Requests::default());
        let worker = || async {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    by_time.store(true, Ordering::Relaxed);
                    break;
                }
                // Started before the decision: a restart cuts it off.
                let first_try = restart.get().is_none();
                let size = if restarted() { small } else { self.size };
                if reserved.fetch_add(size as u64, Ordering::Relaxed) + size as u64 > self.budget {
                    break;
                }
                let payload = probe::payload(seq.fetch_add(1, Ordering::Relaxed) + 1, size);
                let t = Instant::now();
                let req = tokio::time::timeout_at(deadline, request(payload));
                tokio::pin!(req);
                let r = if first_try && small < self.size {
                    // A request that ended (also by the time limit) counts
                    // first.
                    tokio::select! {
                        biased;
                        r = &mut req => r,
                        () = tokio::time::sleep_until(restart_at) => {
                            let decided = restart.get_or_init(|| {
                                (!finished.load(Ordering::Relaxed)).then(Instant::now)
                            });
                            if decided.is_some() {
                                cut_off.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            req.await
                        }
                    }
                } else {
                    req.await
                };
                let Ok(r) = r else {
                    // Cut off by the time limit.
                    results.lock().expect("poisoned").unfinished += 1;
                    by_time.store(true, Ordering::Relaxed);
                    break;
                };
                finished.store(true, Ordering::Relaxed);
                if first_try && restarted() {
                    // Finished just as the others started over.
                    cut_off.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let mut res = results.lock().expect("poisoned");
                match r {
                    Ok(()) => res.rtts.push(t.elapsed()),
                    Err(e) => {
                        res.failed += 1;
                        res.error.get_or_insert(e);
                    }
                }
            }
        };
        n0_future::join_all((0..self.streams).map(|_| worker())).await;
        let end = Instant::now();
        let restarted_at = restart.get().copied().flatten();
        Requests {
            size: if restarted_at.is_some() {
                small
            } else {
                self.size
            },
            by_time: by_time.into_inner(),
            elapsed: end - restarted_at.unwrap_or(start),
            restart: restarted_at.map(|at| (at - start, cut_off.into_inner())),
            ..results.into_inner().expect("poisoned")
        }
    }
}

/// What a successful transfer says the link sustains: the lower of its
/// throughput and that of its second half. On a lossy link a transfer can
/// start fast and settle far lower, and its average then overstates the link
/// (campaign 3, 1 % loss each way: in one run requests sized for an upload's
/// 4.7 Mbps, in another for a download's 6.7 Mbps, and none of them finished
/// in 8 s at the 1-2 Mbps the link then carried).
fn sustained_mbps(t: &TransferReport) -> Option<f64> {
    let m = t.throughput_mbps.filter(|_| t.ok)?;
    Some(t.second_half_mbps.map_or(m, |h| m.min(h)))
}

/// The size of the concurrency phase's requests: `max` (256 KiB, or
/// `--concurrency-size` if smaller), or less on a link measured at
/// `link_mbps` (see [`sustained_mbps`]), so that each of the `streams` can
/// finish about [`REQUESTS_PER_STREAM`] echo requests within `time` (in
/// whole KiB, at least [`MIN_CONCURRENCY_REQUEST`]). Requests still running at the time
/// limit are cut off and not counted, so on a slow link 256 KiB requests
/// gave no throughput at all (campaign 2: 8 x 256 KiB at ~1-4 Mbps).
fn concurrency_request_size(
    max: usize,
    link_mbps: Option<f64>,
    streams: usize,
    time: Duration,
) -> usize {
    let Some(mbps) = link_mbps.filter(|m| m.is_finite() && *m > 0.0) else {
        return max;
    };
    let per_stream = mbps * 1e6 / 8.0 * time.as_secs_f64() / streams.max(1) as f64;
    let fit = (per_stream / REQUESTS_PER_STREAM) as usize / 1024 * 1024;
    fit.clamp(MIN_CONCURRENCY_REQUEST.min(max), max)
}

/// Connects (lookup, QUIC and golem-p2p handshake), timing the lookup
/// separately.
async fn connect(
    ep: &Endpoint,
    to: NodeId,
    discovery: &BenchDiscovery,
) -> (Connect, Option<Connection>) {
    let start = Instant::now();
    let r = ep.connect(to).await;
    let total = start.elapsed();
    let lookup = discovery.lookup_time(to);
    match r {
        Ok(conn) => {
            let p = SelectedPath::of(&conn);
            (
                Connect {
                    ok: true,
                    lookup_ms: lookup.map(ms),
                    connect_ms: Some(ms(total)),
                    handshake_ms: lookup.map(|l| ms(total.saturating_sub(l))),
                    path: path_name(p.kind),
                    remote_addr: p.remote,
                    peer_endpoint: Some(conn.remote_endpoint_id().to_string()),
                    error_kind: None,
                    error: None,
                },
                Some(conn),
            )
        }
        Err(e) => (
            Connect {
                ok: false,
                lookup_ms: lookup.map(ms),
                connect_ms: Some(ms(total)),
                path: "unknown",
                error_kind: Some(error_kind(&e)),
                error: Some(e.to_string()),
                ..Default::default()
            },
            None,
        ),
    }
}

fn latency(pinger: &Pinger, payload_size: usize) -> Latency {
    let s = pinger.summary(payload_size, false);
    Latency {
        duration_ms: ms(s.duration),
        sent: s.sent,
        ok: s.ok,
        failed: s.failed,
        failed_pct: (s.sent > 0).then(|| round3(s.failed as f64 * 100.0 / s.sent as f64)),
        rtt_ms: s.rtt_ms,
        rtt_samples: s.rtt_samples,
        jitter_ms: s.jitter_ms,
        direct_fraction: s.direct_fraction,
        final_path: path_name(s.final_path),
        quic: None,
        loss: None,
    }
}

fn transfer_report(
    requested: u64,
    capped: bool,
    result: anyhow::Result<probe::Transfer>,
) -> TransferReport {
    match result {
        Ok(t) => TransferReport {
            ok: true,
            requested_bytes: requested,
            relay_capped: capped,
            bytes: Some(t.bytes),
            duration_ms: Some(ms(t.elapsed)),
            throughput_mbps: Some(round3(t.mbps)),
            second_half_mbps: t.second_half_mbps.map(round3),
            path: Some(path_name(t.path)),
            limited_by: Some(if t.by_time { "time" } else { "size" }),
            error_kind: None,
            error: None,
            quic: None,
        },
        Err(e) => TransferReport {
            ok: false,
            requested_bytes: requested,
            relay_capped: capped,
            error_kind: Some(probe::transfer_error_kind(&e, "transfer")),
            error: Some(format!("{e:#}")),
            ..Default::default()
        },
    }
}

/// The QUIC counters of a connection at one moment, to measure a phase or a
/// whole run.
struct QuicSnapshot {
    /// [`Connection::stable_id`].
    conn: usize,
    stats: golem_p2p::iroh::endpoint::ConnectionStats,
    /// Path MTU probes sent and lost, summed over the open paths.
    mtu_probes: (u64, u64),
}

impl QuicSnapshot {
    fn of(c: &Connection) -> Self {
        let ic = c.iroh_connection();
        let mtu_probes = ic.paths().iter().fold((0, 0), |(sent, lost), p| {
            let s = p.stats();
            (sent + s.sent_plpmtud_probes, lost + s.lost_plpmtud_probes)
        });
        Self {
            conn: c.stable_id(),
            stats: ic.stats(),
            mtu_probes,
        }
    }

    /// The connection that currently serves `to`, if any.
    fn to(ep: &Endpoint, to: NodeId) -> Option<Self> {
        connection_to(ep, to).map(|c| Self::of(&c))
    }

    /// The counters since `base` (an earlier snapshot of the same
    /// connection), or since the connection started.
    fn since(&self, base: Option<&Self>) -> QuicStats {
        let d = |f: fn(&Self) -> u64| f(self).saturating_sub(base.map_or(0, f));
        let sent = d(|s| s.stats.sent_packets).saturating_sub(d(|s| s.mtu_probes.0));
        let lost = d(|s| s.stats.lost_packets).saturating_sub(d(|s| s.mtu_probes.1));
        QuicStats {
            sent_packets: sent,
            lost_packets: lost,
            lost_pct: (sent > 0).then(|| round3(lost as f64 * 100.0 / sent as f64)),
            sent_bytes: d(|s| s.stats.udp_tx.bytes),
            received_bytes: d(|s| s.stats.udp_rx.bytes),
        }
    }
}

/// The QUIC counters of a phase: of the connection to `to` since `before`,
/// if it is still the same connection.
fn phase_quic(ep: &Endpoint, to: NodeId, before: Option<&QuicSnapshot>) -> Option<QuicStats> {
    let before = before?;
    let now = QuicSnapshot::to(ep, to)?;
    (now.conn == before.conn).then(|| now.since(Some(before)))
}

/// Why a connection closed, as `ConnectionLost::cause`.
fn close_cause(e: &golem_p2p::iroh::endpoint::ConnectionError) -> &'static str {
    use golem_p2p::iroh::endpoint::ConnectionError as E;
    match e {
        E::LocallyClosed => "closed_here",
        E::TimedOut => "idle_timeout",
        E::ApplicationClosed(_) | E::ConnectionClosed(_) => "closed_by_peer",
        E::Reset => "reset",
        _ => "error",
    }
}

fn path_report(ep: &Endpoint, to: NodeId, log: &PathLog, t0: Instant) -> PathReport {
    let final_path = connection_to(ep, to).map(|c| SelectedPath::of(&c));
    PathReport {
        time_to_direct_ms: log
            .first_direct()
            .map(|t| ms(t.saturating_duration_since(t0))),
        changes: log.changes(),
        final_path: path_name(final_path.as_ref().map_or(PathKind::Unknown, |p| p.kind)),
        final_remote_addr: final_path.and_then(|p| p.remote),
        timeline: log
            .timeline()
            .into_iter()
            .map(|e| PathPoint {
                t_ms: ms(e.at.saturating_duration_since(t0)),
                path: path_name(e.path.kind),
                remote_addr: e.path.remote,
            })
            .collect(),
    }
}

/// `10 s`, `200 ms`.
fn fmt_dur(d: Duration) -> String {
    report::fmt_ms(ms(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_count_waits_for_the_path_to_settle() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let settle = PATH_SETTLE;
        let state = |from, change: Option<u64>, now| {
            LossWindow::at(at(from), change.map(at), settle, at(now))
        };
        // No path change: count from the start of the phase.
        assert_eq!(state(0, None, 5000), LossWindow::Counting);
        // A change long before the phase does not matter.
        let t1 = at(5000);
        assert_eq!(
            LossWindow::at(t1, Some(t0), settle, t1 + Duration::from_secs(5)),
            LossWindow::Counting
        );
        // Hole punching finished 90 ms into the phase (campaign 2): wait a
        // second, then count from there.
        assert_eq!(state(0, Some(90), 600), LossWindow::Unsettled);
        assert_eq!(state(0, Some(90), 1090), LossWindow::Restart);
        assert_eq!(state(1100, Some(90), 9000), LossWindow::Counting);
        // A change just before the phase started is waited out too.
        let t1 = at(300);
        assert_eq!(
            LossWindow::at(t1, Some(at(200)), settle, at(700)),
            LossWindow::Unsettled
        );
        // Another change after counting started: start again once settled.
        assert_eq!(state(1100, Some(4000), 4500), LossWindow::Unsettled);
        assert_eq!(state(1100, Some(4000), 5000), LossWindow::Restart);
        // A change in the last second of the phase: never settled.
        assert_eq!(state(1100, Some(9500), 10_000), LossWindow::Unsettled);

        // The path must be unchanged for 1 s, or 3 of the shortest round
        // trips on long paths.
        let mut c = LossCount {
            start: t0,
            from: t0,
            base: None,
            min_rtt: None,
        };
        assert_eq!(c.settle(), PATH_SETTLE);
        c.min_rtt = Some(Duration::from_millis(80));
        assert_eq!(c.settle(), PATH_SETTLE);
        c.min_rtt = Some(Duration::from_millis(600));
        assert_eq!(c.settle(), Duration::from_millis(1800));
        c.min_rtt = Some(Duration::from_secs(30));
        assert_eq!(c.settle(), Duration::from_secs(10));
    }

    #[test]
    fn sustained_rates() {
        let t = |mbps: f64, half: Option<f64>| TransferReport {
            ok: true,
            throughput_mbps: Some(mbps),
            second_half_mbps: half,
            ..Default::default()
        };
        // Fast or short: the average.
        assert_eq!(sustained_mbps(&t(123.0, None)), Some(123.0));
        assert_eq!(sustained_mbps(&t(30.0, Some(45.0))), Some(30.0));
        // A fast start that settled lower: the second half.
        assert_eq!(sustained_mbps(&t(4.7, Some(1.1))), Some(1.1));
        // Failed: nothing.
        let failed = TransferReport {
            ok: false,
            ..t(4.7, None)
        };
        assert_eq!(sustained_mbps(&failed), None);
    }

    /// Workers for 600 ms, without a budget to speak of.
    fn workers(streams: usize, size: usize) -> Workers {
        Workers {
            streams,
            budget: 1 << 40,
            size,
            time: Duration::from_millis(600),
        }
    }

    /// A link on which requests of at most `max` bytes take 10 ms, and larger
    /// ones never finish.
    async fn link(payload: Vec<u8>, max: usize) -> anyhow::Result<()> {
        if payload.len() > max {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(())
    }

    #[test]
    fn restart_sizes() {
        let size = |s| workers(8, s).restart_size();
        assert_eq!(size(256 * 1024), 16 * 1024);
        // Campaign 3, debug/lossy-extra run 10: 204 KiB -> 12 KiB.
        assert_eq!(size(208_896), 12 * 1024);
        assert_eq!(size(40 * 1024), 4 * 1024);
        // Nothing smaller to start over with.
        assert_eq!(size(4 * 1024), 4 * 1024);
        assert_eq!(size(1000), 1000);
    }

    /// None of the first requests finished by half the time: the workers cut
    /// them off and start over with smaller ones, which are counted from then.
    #[tokio::test]
    async fn workers_start_over_with_smaller_requests() {
        let ms = Duration::from_millis;
        let r = workers(4, 256 * 1024).run(|p| link(p, 16 * 1024)).await;
        let (at, cut_off) = r.restart.unwrap();
        // At half the time (give or take a busy test machine).
        assert!(at >= ms(300) && at < ms(500), "{at:?}");
        assert_eq!(cut_off, 4);
        assert_eq!(r.size, 16 * 1024);
        assert!(r.rtts.len() >= 8, "{}", r.rtts.len());
        assert_eq!(r.failed, 0);
        assert!(r.by_time && r.unfinished <= 4);
        // Counted from the restart to the end (600 ms).
        let rest = r.elapsed + at;
        assert!(rest >= ms(600) && rest < ms(800), "{:?}", r.elapsed);
    }

    /// One request finished in the first half: no restart, also if all
    /// others hang until the time limit.
    #[tokio::test]
    async fn workers_go_on_after_a_finished_request() {
        let first = AtomicBool::new(true);
        let r = workers(4, 256 * 1024)
            .run(|p| {
                let max = if first.swap(false, Ordering::Relaxed) {
                    usize::MAX
                } else {
                    0
                };
                link(p, max)
            })
            .await;
        assert!(r.restart.is_none());
        assert_eq!((r.size, r.rtts.len(), r.unfinished), (256 * 1024, 1, 4));
        assert!(r.by_time);
        assert!(r.elapsed >= Duration::from_millis(600), "{:?}", r.elapsed);
    }

    /// Requests that cannot get smaller are not restarted; fast requests end
    /// with the budget.
    #[tokio::test]
    async fn workers_without_restart() {
        let r = workers(2, 4096).run(|p| link(p, 0)).await;
        assert!(r.restart.is_none());
        assert_eq!((r.rtts.len(), r.unfinished), (0, 2));
        let w = Workers {
            budget: 10 * 4096,
            ..workers(2, 4096)
        };
        let r = w.run(|p| link(p, 4096)).await;
        assert!(r.restart.is_none() && !r.by_time);
        assert_eq!((r.rtts.len(), r.unfinished), (10, 0));
        assert!(r.elapsed < Duration::from_millis(300), "{:?}", r.elapsed);
    }

    #[test]
    fn concurrency_requests_fit_the_link() {
        let size = |mbps: Option<f64>| {
            concurrency_request_size(CONCURRENCY_REQUEST, mbps, 8, Duration::from_secs(8))
        };
        // Fast links, or nothing measured: 256 KiB.
        assert_eq!(size(None), CONCURRENCY_REQUEST);
        assert_eq!(size(Some(123.0)), CONCURRENCY_REQUEST);
        assert_eq!(size(Some(f64::NAN)), CONCURRENCY_REQUEST);
        // From about 8.4 Mbps down, 4 requests per stream fit in 8 s.
        assert_eq!(size(Some(8.4)), CONCURRENCY_REQUEST);
        assert_eq!(size(Some(8.0)), 244 * 1024);
        // Campaign 2's lossy link: 3.7 and 1.1 Mbps (the slower direction).
        assert_eq!(size(Some(3.658)), 111 * 1024);
        assert_eq!(size(Some(1.1)), 33 * 1024);
        // Very slow: at least 4 KiB.
        assert_eq!(size(Some(0.05)), MIN_CONCURRENCY_REQUEST);
        // Never more than --concurrency-size.
        assert_eq!(
            concurrency_request_size(1000, Some(0.05), 8, Duration::from_secs(8)),
            1000
        );
    }
}
