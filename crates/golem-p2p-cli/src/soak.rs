//! Soak testing: the `soak` client, and the periodic `stats` events of
//! `listen` and `directory` (`--stats-interval`).
//!
//! `soak` runs for hours against a `listen` node: one echo request per
//! `--interval`, optionally an upload every `--bulk-every`, and every
//! `--stats-interval` one `soak_stats` event with the window's request
//! counts, errors, round-trip times (percentiles and a histogram that a
//! harness can merge over many clients), uploads, and the process' own
//! resources. Started for a few seconds again and again it is the client of
//! churn tests. The `stats` events report the resources and internal state of
//! a long-running listener or directory, so a harness can see growth (memory,
//! file descriptors, threads, tasks, connection tables) over time.

use std::{
    collections::BTreeMap,
    pin::pin,
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use golem_p2p::{Endpoint, NodeId, PathKind, RecordStore};
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

use crate::{
    Node, NodeArgs,
    listen::{self, SINK_MAGIC},
    observe::{connection_to, path_to},
    out::{Out, anyhow_kind, ms, path_name, round3},
    parse,
};

/// Arguments of `soak`.
#[derive(Args)]
pub struct SoakArgs {
    #[command(flatten)]
    node: NodeArgs,
    /// Node ID of the `listen` node.
    #[arg(long)]
    to: NodeId,
    /// How long to run: `30s`, `2h`; a plain number is seconds. Default:
    /// until SIGINT or SIGTERM.
    #[arg(long, value_parser = parse::duration)]
    duration: Option<Duration>,
    /// Time between the starts of two echo requests (one at a time; a
    /// request that takes longer delays the next).
    #[arg(long, default_value = "1s", value_parser = parse::duration)]
    interval: Duration,
    /// Echo request size in bytes (at most 1 MiB).
    #[arg(long, default_value = "64", value_parser = parse::bytes)]
    payload_size: u64,
    /// Timeout for connecting and for each echo request.
    #[arg(long, default_value = "10s", value_parser = parse::duration)]
    timeout: Duration,
    /// Upload `--bulk-size` bytes to the listener this often, alongside the
    /// echo requests (the first after one period, at most one at a time).
    #[arg(long, value_parser = parse::duration)]
    bulk_every: Option<Duration>,
    /// Size of each upload, e.g. `1M`.
    #[arg(long, default_value = "1M", value_parser = parse::bytes)]
    bulk_size: u64,
    /// Timeout of one upload.
    #[arg(long, default_value = "60s", value_parser = parse::duration)]
    bulk_timeout: Duration,
    /// Emit a `soak_stats` event this often (and always one at the end).
    #[arg(long, default_value = "30s", value_parser = parse::duration)]
    stats_interval: Duration,
}

/// Upper bounds (ms) of the round-trip time histogram buckets in
/// `soak_stats`; a last bucket `inf` takes the rest.
pub const RTT_BUCKETS_MS: &[f64] = &[
    0.1, 0.2, 0.3, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 7.5, 10.0, 15.0, 20.0, 30.0, 50.0, 75.0,
    100.0, 150.0, 200.0, 300.0, 500.0, 750.0, 1000.0, 2000.0, 5000.0, 10000.0,
];

/// The outcome of one echo request.
struct EchoResult {
    /// The request needed a new connection (lookup, dial, handshake), so its
    /// time is not a round-trip time.
    new_conn: bool,
    rtt: Duration,
    path: PathKind,
    result: anyhow::Result<()>,
}

/// The outcome of one upload.
struct BulkResult {
    bytes: u64,
    elapsed: Duration,
    result: anyhow::Result<()>,
}

/// Counts and times of one `soak_stats` window.
struct Window {
    start: Instant,
    sent: u64,
    ok: u64,
    failed: u64,
    new_conns: u64,
    ok_direct: u64,
    errors: BTreeMap<&'static str, u64>,
    /// Round-trip times of successful requests on an existing connection.
    rtts: Vec<Duration>,
    bulk_ok: u64,
    bulk_failed: u64,
    bulk_bytes: u64,
    bulk_time: Duration,
}

impl Window {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            sent: 0,
            ok: 0,
            failed: 0,
            new_conns: 0,
            ok_direct: 0,
            errors: BTreeMap::new(),
            rtts: Vec::new(),
            bulk_ok: 0,
            bulk_failed: 0,
            bulk_bytes: 0,
            bulk_time: Duration::ZERO,
        }
    }

    fn echo(&mut self, r: &EchoResult, out: &Out) {
        self.sent += 1;
        if r.new_conn {
            self.new_conns += 1;
        }
        match &r.result {
            Ok(()) => {
                self.ok += 1;
                if r.path == PathKind::Direct {
                    self.ok_direct += 1;
                }
                if !r.new_conn {
                    self.rtts.push(r.rtt);
                }
            }
            Err(e) => {
                self.failed += 1;
                let kind = echo_error_kind(e);
                *self.errors.entry(kind).or_default() += 1;
                out.event("error")
                    .field("kind", kind)
                    .field("message", format!("echo request: {e:#}"))
                    .field("elapsed_ms", ms(r.rtt))
                    .emit();
            }
        }
    }

    fn bulk(&mut self, r: &BulkResult, out: &Out) {
        match &r.result {
            Ok(()) => {
                self.bulk_ok += 1;
                self.bulk_bytes += r.bytes;
                self.bulk_time += r.elapsed;
            }
            Err(e) => {
                self.bulk_failed += 1;
                *self.errors.entry("bulk").or_default() += 1;
                out.event("error")
                    .field("kind", "bulk")
                    .field("message", format!("upload of {} bytes: {e:#}", r.bytes))
                    .field("elapsed_ms", ms(r.elapsed))
                    .emit();
            }
        }
    }
}

/// Totals over the whole run.
#[derive(Default)]
struct Totals {
    sent: u64,
    ok: u64,
    failed: u64,
    new_conns: u64,
    bulk_ok: u64,
    bulk_failed: u64,
    bulk_bytes: u64,
}

impl Totals {
    fn add(&mut self, w: &Window) {
        self.sent += w.sent;
        self.ok += w.ok;
        self.failed += w.failed;
        self.new_conns += w.new_conns;
        self.bulk_ok += w.bulk_ok;
        self.bulk_failed += w.bulk_failed;
        self.bulk_bytes += w.bulk_bytes;
    }
}

fn echo_error_kind(e: &anyhow::Error) -> &'static str {
    if e.to_string().starts_with("unexpected reply") {
        return "mismatch";
    }
    anyhow_kind(e, "request")
}

/// Payload of request `seq`: an ASCII marker padded to `size` bytes.
fn payload(seq: u64, size: usize) -> Vec<u8> {
    let mut p = format!("soak seq={seq} ").into_bytes();
    if p.len() < size {
        p.resize(size, b'x');
    }
    p
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[Duration], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    Some(ms(sorted[rank.clamp(1, sorted.len()) - 1]))
}

/// Percentiles of the round-trip times, in ms.
fn rtt_stats(rtts: &[Duration]) -> Value {
    let mut sorted = rtts.to_vec();
    sorted.sort();
    let mean =
        (!sorted.is_empty()).then(|| ms(sorted.iter().sum::<Duration>() / sorted.len() as u32));
    json!({
        "p50": percentile(&sorted, 50.0),
        "p90": percentile(&sorted, 90.0),
        "p99": percentile(&sorted, 99.0),
        "max": percentile(&sorted, 100.0),
        "mean": mean,
    })
}

/// The round-trip times as a histogram over [`RTT_BUCKETS_MS`]: upper bound
/// (`inf` for the last bucket) to count, only buckets with samples.
fn rtt_histogram(rtts: &[Duration]) -> Value {
    let mut counts = vec![0u64; RTT_BUCKETS_MS.len() + 1];
    for r in rtts {
        let v = r.as_secs_f64() * 1000.0;
        let i = RTT_BUCKETS_MS
            .iter()
            .position(|b| v <= *b)
            .unwrap_or(RTT_BUCKETS_MS.len());
        counts[i] += 1;
    }
    let mut m = serde_json::Map::new();
    for (i, c) in counts.into_iter().enumerate() {
        if c > 0 {
            let key = RTT_BUCKETS_MS
                .get(i)
                .map_or("inf".to_string(), |b| b.to_string());
            m.insert(key, c.into());
        }
    }
    Value::Object(m)
}

/// Uploads `bytes` to the listener (its sink) and checks the count it
/// replies with.
async fn upload(ep: &Endpoint, to: NodeId, bytes: u64) -> anyhow::Result<()> {
    let (mut send, mut recv) = ep.open_bi(to).await?;
    let write = async {
        send.write_all(SINK_MAGIC).await?;
        let chunk = vec![0u8; 64 * 1024];
        let mut left = bytes;
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            send.write_all(&chunk[..n]).await?;
            left -= n as u64;
        }
        send.finish()?;
        Ok::<_, anyhow::Error>(())
    };
    let written = write.await;
    // On a refusal the listener stops the upload and replies with the reason.
    let reply = tokio::time::timeout(Duration::from_secs(10), recv.read_to_end(4096)).await;
    let text = match reply {
        Ok(Ok(r)) => String::from_utf8_lossy(&r).into_owned(),
        Ok(Err(e)) => {
            written?;
            return Err(e).context("reading the listener's reply");
        }
        Err(_) => {
            written?;
            bail!("no reply from the listener within 10 s");
        }
    };
    if text.starts_with("refused") {
        bail!("listener refused the upload: {}", text.trim());
    }
    written?;
    let got = text
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("sink_bytes="))
        .and_then(|v| v.parse::<u64>().ok())
        .with_context(|| format!("unexpected reply: {}", text.trim()))?;
    if got != bytes {
        bail!("the listener received {got} of {bytes} bytes");
    }
    Ok(())
}

/// Sends one echo request per `interval` until aborted, reporting each.
async fn requester(
    ep: Endpoint,
    to: NodeId,
    interval: Duration,
    payload_size: usize,
    tx: mpsc::Sender<EchoResult>,
) {
    // Spread the requests of many clients over the interval.
    let offset = interval.mul_f64(f64::from(ep.instance_id()[0]) / 256.0);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + offset, interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq = 0u64;
    loop {
        tick.tick().await;
        seq += 1;
        let data = payload(seq, payload_size);
        let new_conn = connection_to(&ep, to).is_none();
        let start = Instant::now();
        let result = match ep.request(to, &data).await {
            Ok(r) if r.ends_with(&data) => Ok(()),
            Ok(r) => Err(anyhow!(
                "unexpected reply: {}",
                String::from_utf8_lossy(&r[..r.len().min(200)])
            )),
            Err(e) => Err(e.into()),
        };
        let r = EchoResult {
            new_conn,
            rtt: start.elapsed(),
            path: path_to(&ep, to),
            result,
        };
        if tx.send(r).await.is_err() {
            break;
        }
    }
}

/// Aborts a task when dropped.
pub struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `soak`.
pub async fn run(args: SoakArgs, out: &Out) -> anyhow::Result<ExitCode> {
    if args.interval.is_zero() || args.stats_interval.is_zero() {
        bail!("--interval and --stats-interval must be positive");
    }
    if args.bulk_every.is_some_and(|d| d.is_zero()) {
        bail!("--bulk-every must be positive");
    }
    let payload_size = usize::try_from(args.payload_size)
        .ok()
        .filter(|s| *s <= listen::MAX_REQUEST)
        .with_context(|| format!("--payload-size must be at most {}", listen::MAX_REQUEST))?;
    let run_start = Instant::now();
    let node = Node::start(&args.node, out, Some(args.timeout), None).await?;
    let bind_ms = ms(run_start.elapsed());
    let ep = node.ep.clone();
    let to = args.to;
    let stopped = crate::stop_signal();
    let mut stop = pin!(stopped());
    let end = args.duration.map(|d| tokio::time::Instant::now() + d);
    let mut end_sleep =
        pin!(tokio::time::sleep_until(end.unwrap_or_else(
            || tokio::time::Instant::now() + Duration::from_secs(100 * 365 * 86_400)
        )));

    let (echo_tx, mut echo_rx) = mpsc::channel(16);
    let requests = AbortOnDrop(tokio::spawn(requester(
        ep.clone(),
        to,
        args.interval,
        payload_size,
        echo_tx,
    )));
    let (bulk_tx, mut bulk_rx) = mpsc::channel::<BulkResult>(1);
    let mut bulk_task: Option<AbortOnDrop> = None;
    let now = tokio::time::Instant::now();
    let mut stats_tick = tokio::time::interval_at(now + args.stats_interval, args.stats_interval);
    stats_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let bulk_every = args.bulk_every.unwrap_or(Duration::from_secs(3600));
    let mut bulk_tick = tokio::time::interval_at(now + bulk_every, bulk_every);
    bulk_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let session = Session {
        bind_ms,
        start: run_start,
        first_ok_ms: None,
    };
    let mut report = Report {
        out: out.clone(),
        ep: ep.clone(),
        to,
        session,
        totals: Totals::default(),
    };
    let mut window = Window::new();
    let mut interrupted = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                interrupted = true;
                break;
            }
            _ = &mut end_sleep => break,
            _ = stats_tick.tick() => {
                report.emit(&window, false, false);
                window = Window::new();
            }
            Some(r) = bulk_rx.recv() => {
                bulk_task = None;
                window.bulk(&r, out);
            }
            _ = bulk_tick.tick(), if args.bulk_every.is_some() && bulk_task.is_none() => {
                let (ep, tx) = (ep.clone(), bulk_tx.clone());
                let (bytes, timeout) = (args.bulk_size, args.bulk_timeout);
                bulk_task = Some(AbortOnDrop(tokio::spawn(async move {
                    let start = Instant::now();
                    let result = tokio::time::timeout(timeout, upload(&ep, to, bytes))
                        .await
                        .unwrap_or_else(|_| Err(anyhow::Error::from(golem_p2p::Error::Timeout)));
                    let _ = tx.send(BulkResult { bytes, elapsed: start.elapsed(), result }).await;
                })));
            }
            Some(r) = echo_rx.recv() => {
                if r.result.is_ok() && report.session.first_ok_ms.is_none() {
                    report.session.first_ok_ms = Some(ms(run_start.elapsed()));
                }
                window.echo(&r, out);
            }
        }
    }
    drop(requests);
    drop(bulk_task);
    report.emit(&window, true, interrupted);
    let totals = &report.totals;
    let code = if totals.ok == 0 {
        ExitCode::from(1)
    } else if totals.failed > 0 || totals.bulk_failed > 0 {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    };
    if !out.json() {
        println!(
            "sent={} ok={} failed={} new_conns={} bulk_ok={} bulk_failed={}",
            totals.sent,
            totals.ok,
            totals.failed,
            totals.new_conns,
            totals.bulk_ok,
            totals.bulk_failed
        );
    }
    ep.close().await;
    Ok(code)
}

/// Facts about this run of `soak`, repeated in every `soak_stats` event.
struct Session {
    bind_ms: f64,
    start: Instant,
    /// Time from start to the first successful reply.
    first_ok_ms: Option<f64>,
}

/// Emits `soak_stats` events.
struct Report {
    out: Out,
    ep: Endpoint,
    to: NodeId,
    session: Session,
    totals: Totals,
}

impl Report {
    fn emit(&mut self, w: &Window, last: bool, interrupted: bool) {
        self.totals.add(w);
        let t = &self.totals;
        let mbps = (w.bulk_time > Duration::ZERO)
            .then(|| round3(w.bulk_bytes as f64 * 8.0 / w.bulk_time.as_secs_f64() / 1e6));
        let ev = self
            .out
            .event("soak_stats")
            .field("to", self.to.to_string())
            .field("window_ms", ms(w.start.elapsed()))
            .field("sent", w.sent)
            .field("ok", w.ok)
            .field("failed", w.failed)
            .field("new_conns", w.new_conns)
            .field(
                "direct_fraction",
                (w.ok > 0).then(|| round3(w.ok_direct as f64 / w.ok as f64)),
            )
            .field("errors", json!(w.errors))
            .field("rtt_ms", rtt_stats(&w.rtts))
            .field("rtt_hist", rtt_histogram(&w.rtts))
            .field(
                "bulk",
                json!({"ok": w.bulk_ok, "failed": w.bulk_failed, "bytes": w.bulk_bytes, "mbps": mbps}),
            )
            .field(
                "totals",
                json!({
                    "sent": t.sent, "ok": t.ok, "failed": t.failed, "new_conns": t.new_conns,
                    "bulk_ok": t.bulk_ok, "bulk_failed": t.bulk_failed, "bulk_bytes": t.bulk_bytes,
                }),
            )
            .field("path", path_name(path_to(&self.ep, self.to)))
            .field(
                "session",
                json!({
                    "uptime_s": round3(self.session.start.elapsed().as_secs_f64()),
                    "bind_ms": self.session.bind_ms,
                    "first_ok_ms": self.session.first_ok_ms,
                }),
            )
            .field("proc", proc_stats())
            .field("tokio", tokio_stats())
            .field("endpoint", endpoint_stats(&self.ep))
            .field("final", last)
            .field("interrupted", interrupted);
        if last {
            ev.emit_last();
        } else {
            ev.emit();
        }
    }
}

/// Resources of this process from `/proc/self` (Linux; fields are null
/// elsewhere): resident memory and its peak, anonymous resident memory
/// (mostly heap), threads, open file descriptors (and how many are sockets),
/// and CPU time used (user + system).
pub fn proc_stats() -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
    };
    let (mut fds, mut sockets) = (None, None);
    if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
        let (mut n, mut s) = (0u64, 0u64);
        for e in dir.flatten() {
            n += 1;
            if std::fs::read_link(e.path())
                .is_ok_and(|t| t.to_string_lossy().starts_with("socket:"))
            {
                s += 1;
            }
        }
        fds = Some(n);
        sockets = Some(s);
    }
    let heap = heap_kb();
    json!({
        "rss_kb": field("VmRSS"),
        "hwm_kb": field("VmHWM"),
        "anon_kb": field("RssAnon"),
        "heap_used_kb": heap.map(|h| h.0),
        "heap_free_kb": heap.map(|h| h.1),
        "threads": field("Threads"),
        "fds": fds,
        "sockets": sockets,
        "cpu_s": cpu_seconds(),
    })
}

/// Heap in use and free heap held by the allocator, in KiB (glibc only;
/// `None` elsewhere). In use is what the program has allocated and not freed
/// (including large blocks mapped separately); free is memory the allocator
/// keeps for reuse. Growth of the first is a leak, growth of the second only
/// fragmentation or retention.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn heap_kb() -> Option<(u64, u64)> {
    #[repr(C)]
    struct Mallinfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> Mallinfo2;
    }
    // SAFETY: mallinfo2 (glibc >= 2.33) takes no arguments and returns a
    // plain struct; it locks the allocator's arenas internally.
    let m = unsafe { mallinfo2() };
    Some((
        ((m.uordblks + m.hblkhd) / 1024) as u64,
        (m.fordblks / 1024) as u64,
    ))
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn heap_kb() -> Option<(u64, u64)> {
    None
}

/// User plus system CPU time of this process, from `/proc/self/stat`
/// (assuming the usual 100 clock ticks per second).
fn cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the command name, which is in parentheses and may contain
    // spaces: state is field 3, utime and stime are fields 14 and 15.
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(round3((utime + stime) as f64 / 100.0))
}

/// The tokio runtime: worker threads, tasks alive (spawned and not yet
/// finished), and tasks waiting in the global queue.
pub fn tokio_stats() -> Value {
    let m = tokio::runtime::Handle::current().metrics();
    json!({
        "workers": m.num_workers(),
        "alive_tasks": m.num_alive_tasks(),
        "global_queue": m.global_queue_depth(),
    })
}

/// The endpoint's internal state and counters ([`golem_p2p::EndpointStats`]),
/// plus open connections and a few of iroh's counters.
pub fn endpoint_stats(ep: &Endpoint) -> Value {
    let s = ep.stats();
    let m = &ep.iroh().metrics().socket;
    json!({
        "open_connections": ep.connections().len(),
        "connections": s.connections,
        "connect_slots": s.connect_slots,
        "dial_locks": s.dial_locks,
        "handshakes": s.handshakes,
        "queued_streams": s.queued_streams,
        "cached_identities": s.cached_identities,
        "cached_endpoints": s.cached_endpoints,
        "connections_outgoing": s.connections_outgoing,
        "connections_incoming": s.connections_incoming,
        "connections_closed": s.connections_closed,
        "handshakes_failed": s.handshakes_failed,
        "streams_accepted": s.streams_accepted,
        "streams_refused": s.streams_refused,
        "iroh": {
            "conns_opened": m.num_conns_opened.get(),
            "conns_closed": m.num_conns_closed.get(),
            "conns_direct": m.num_conns_direct.get(),
            "paths_direct": m.paths_direct.get(),
            "paths_relay": m.paths_relay.get(),
            "holepunch_attempts": m.holepunch_attempts.get(),
            "ip_paths_added": m.transport_ip_paths_added.get(),
            "ip_paths_removed": m.transport_ip_paths_removed.get(),
            "relay_paths_added": m.transport_relay_paths_added.get(),
            "relay_paths_removed": m.transport_relay_paths_removed.get(),
            "relay_conns_success": m.relay_conns_success.get(),
            "relay_conns_failed": m.relay_conns_failed.get(),
            "relay_conns_closed": m.relay_conns_closed.get(),
        },
    })
}

/// What a `stats` event reports besides the process and the runtime.
#[derive(Clone)]
pub enum StatsSource {
    /// A `listen` node.
    Endpoint(Endpoint),
    /// A `directory` and its record store.
    Directory(Arc<RecordStore>),
}

/// Emits a `stats` event every `interval` until dropped.
pub fn spawn_stats(out: &Out, interval: Duration, source: StatsSource) -> AbortOnDrop {
    let out = out.clone();
    let start = Instant::now();
    AbortOnDrop(tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let mut ev = out
                .event("stats")
                .field("uptime_s", round3(start.elapsed().as_secs_f64()))
                .field("proc", proc_stats())
                .field("tokio", tokio_stats());
            ev = match &source {
                StatsSource::Endpoint(ep) => ev.field("endpoint", endpoint_stats(ep)),
                StatsSource::Directory(store) => {
                    let (ids, eps) = store.len();
                    ev.field("store", json!({"identities": ids, "endpoints": eps}))
                }
            };
            ev.emit();
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets() {
        let d = |us: u64| Duration::from_micros(us);
        let h = rtt_histogram(&[d(50), d(100), d(101), d(900), d(20_000_000)]);
        assert_eq!(h, json!({"0.1": 2, "0.2": 1, "1": 1, "inf": 1}));
        assert_eq!(rtt_histogram(&[]), json!({}));
    }

    #[test]
    fn rtt_percentiles() {
        let rtts: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let s = rtt_stats(&rtts);
        assert_eq!(s["p50"], json!(50.0));
        assert_eq!(s["p90"], json!(90.0));
        assert_eq!(s["p99"], json!(99.0));
        assert_eq!(s["max"], json!(100.0));
        assert_eq!(rtt_stats(&[])["p50"], Value::Null);
    }

    #[test]
    fn payloads() {
        assert_eq!(payload(7, 4), b"soak seq=7 ".to_vec());
        let p = payload(7, 64);
        assert_eq!(p.len(), 64);
        assert!(p.starts_with(b"soak seq=7 x"));
    }

    #[tokio::test]
    async fn own_process_stats() {
        let p = proc_stats();
        if cfg!(target_os = "linux") {
            assert!(p["rss_kb"].as_u64().unwrap() > 0, "{p}");
            assert!(p["threads"].as_u64().unwrap() >= 1, "{p}");
            assert!(p["fds"].as_u64().unwrap() >= 3, "{p}");
            assert!(p["cpu_s"].as_f64().is_some(), "{p}");
        }
        assert!(tokio_stats()["alive_tasks"].as_u64().is_some());
    }
}
