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
    Concurrency, Connect, ConnectionLost, Idle, Latency, PathPoint, PathReport, Peer, QuicStats,
    RelayCompare, Report, Settings, Status, TransferReport,
};
use crate::{
    Node, NodeArgs, listen,
    observe::{PathLog, SelectedPath, connection_to, path_to},
    out::{Out, anyhow_kind, error_kind, ms, path_name, round3},
    parse,
    probe::{self, Pinger, Stats},
};

/// Size of each request of the concurrency phase.
const CONCURRENCY_REQUEST: usize = 256 * 1024;

/// The relay-only comparison measures latency for at most this long.
const RELAY_LATENCY_MAX: Duration = Duration::from_secs(5);

/// Requests after the idle time before giving up.
const IDLE_ATTEMPTS: u64 = 3;

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
    #[arg(long, default_value = "10s", value_parser = parse::duration)]
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
            let mut pinger = Pinger::new(ep, to, out, log.clone()).with_phase("latency");
            let interrupted = probe::ping_series(
                &mut pinger,
                args.latency_duration,
                args.interval,
                self.payload_size,
                args.timeout,
                stop,
                |_, _| {},
            )
            .await;
            let mut l = latency(&pinger, self.payload_size);
            l.quic = phase_quic(ep, to, before.as_ref());
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
            let Some(c) = unless_stopped(self.concurrency(), stop).await else {
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
        let limit = args.transfer_time + args.timeout;
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

    /// `--streams` parallel workers sending echo requests of 256 KiB until
    /// `--concurrency-size` bytes or `--transfer-time`, while small requests
    /// measure the round-trip time under load.
    async fn concurrency(&self) -> Concurrency {
        let args = self.args;
        let (ep, to) = (self.ep, self.to);
        let relayed = path_to(ep, to) == PathKind::Relay;
        let budget = if relayed {
            args.concurrency_size.min(args.relay_transfer_size)
        } else {
            args.concurrency_size
        };
        let size = args.request_size();
        self.progress.say(format!(
            "Sending {} in parallel over {} streams (at most {})...",
            report::fmt_bytes(budget),
            args.streams,
            fmt_dur(args.transfer_time)
        ));
        let before = QuicSnapshot::to(ep, to);
        let start = Instant::now();
        let deadline = start + args.transfer_time;
        let reserved = AtomicU64::new(0);
        let seq = AtomicU64::new(0);
        let done = AtomicBool::new(false);
        let results = Mutex::new((Vec::new(), 0u64, None::<anyhow::Error>));
        let worker = || async {
            loop {
                if Instant::now() >= deadline {
                    break;
                }
                if reserved.fetch_add(size as u64, Ordering::Relaxed) + size as u64 > budget {
                    break;
                }
                let payload = probe::payload(seq.fetch_add(1, Ordering::Relaxed) + 1, size);
                let t = Instant::now();
                let r = ep.request(to, &payload).await;
                let mut res = results.lock().expect("poisoned");
                match r {
                    Ok(reply) if reply.ends_with(&payload) => res.0.push(t.elapsed()),
                    Ok(_) => {
                        res.1 += 1;
                        res.2
                            .get_or_insert_with(|| anyhow::anyhow!("unexpected reply"));
                    }
                    Err(e) => {
                        res.1 += 1;
                        res.2.get_or_insert(e.into());
                    }
                }
            }
        };
        let workers = async {
            n0_future::join_all((0..args.streams).map(|_| worker())).await;
            done.store(true, Ordering::Relaxed);
        };
        let small = async {
            let mut rtts = Vec::new();
            let mut tick = tokio::time::interval(args.interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut n = 0;
            while !done.load(Ordering::Relaxed) {
                tick.tick().await;
                if done.load(Ordering::Relaxed) {
                    break;
                }
                n += 1;
                let payload = probe::payload(n, self.payload_size);
                let t = Instant::now();
                let r = tokio::select! {
                    r = ep.request(to, &payload) => Some(r),
                    _ = wait_flag(&done) => None,
                };
                match r {
                    Some(Ok(reply)) if reply.ends_with(&payload) => rtts.push(t.elapsed()),
                    _ => {}
                }
            }
            rtts
        };
        let ((), small) = tokio::join!(workers, small);
        let elapsed = start.elapsed();
        let (rtts, failed, error) = results.into_inner().expect("poisoned");
        let total = rtts.len() as u64 * size as u64;
        Concurrency {
            ok: failed == 0 && !rtts.is_empty(),
            streams: args.streams,
            request_bytes: size,
            requests: rtts.len() as u64,
            failed,
            total_bytes: total,
            duration_ms: ms(elapsed),
            throughput_mbps: (!rtts.is_empty()).then(|| round3(probe::mbps(total, elapsed))),
            rtt_ms: Stats::of(&rtts),
            loaded_rtt_ms: Stats::of(&small),
            loaded_samples: small.len(),
            error_kind: error.as_ref().map(|e| anyhow_kind(e, "request")),
            error: error.map(|e| format!("{e:#}")),
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
            let mut pinger = Pinger::new(ep, self.to, self.out, log).with_phase("relay_latency");
            probe::ping_series(
                &mut pinger,
                args.latency_duration.min(RELAY_LATENCY_MAX),
                args.interval,
                self.payload_size,
                args.timeout,
                stop,
                |_, _| {},
            )
            .await;
            let mut l = latency(&pinger, self.payload_size);
            l.quic = phase_quic(ep, self.to, Some(&base));
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
            rc.ok = connected
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

async fn wait_flag(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `10 s`, `200 ms`.
fn fmt_dur(d: Duration) -> String {
    report::fmt_ms(ms(d))
}
