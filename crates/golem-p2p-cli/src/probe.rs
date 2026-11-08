//! Request/reply measurement shared by `send`, `probe` and `bench`, and the
//! uploads and downloads of `probe --bulk` and `bench`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use golem_p2p::{Endpoint, NodeId, PathKind, RecvStream};
use serde::Serialize;
use serde_json::Value;

use crate::{
    listen::{REFUSED, SINK_MAGIC, SOURCE_MAGIC, UPLOAD_STOPPED},
    observe::{PathLog, connection_to, path_rtt, path_to},
    out::{Out, anyhow_kind, ms, path_name, round3},
};

/// Counters and timings of a series of requests to one peer.
pub struct Pinger {
    ep: Endpoint,
    to: NodeId,
    out: Out,
    /// Added to every `reply` event as `phase` (bench only).
    phase: Option<&'static str>,
    /// When the first request was started. All `*_ms` times count from here.
    t0: Instant,
    path_log: Arc<PathLog>,
    sent: u64,
    ok: u64,
    ok_direct: u64,
    /// Round-trip times of successful requests on an existing connection,
    /// in the order they were measured.
    rtts: Vec<Duration>,
    connect: Option<Duration>,
}

/// The outcome of one request.
pub struct Reply {
    pub seq: u64,
    pub path: PathKind,
    pub rtt: Duration,
    pub result: anyhow::Result<Vec<u8>>,
}

impl Pinger {
    pub fn new(ep: &Endpoint, to: NodeId, out: &Out, path_log: Arc<PathLog>) -> Self {
        Self {
            ep: ep.clone(),
            to,
            out: out.clone(),
            phase: None,
            t0: Instant::now(),
            path_log,
            sent: 0,
            ok: 0,
            ok_direct: 0,
            rtts: Vec::new(),
            connect: None,
        }
    }

    /// Labels the `reply` events with a bench phase.
    pub fn with_phase(mut self, phase: &'static str) -> Self {
        self.phase = Some(phase);
        self
    }

    /// Sends one request, waits for the reply (unless `cancel` resolves
    /// first, with the error to report), records it and emits a `reply`
    /// event. `check` decides whether a received reply counts as correct.
    /// `with_text` adds the reply text to the event.
    pub async fn ping(
        &mut self,
        payload: &[u8],
        cancel: impl Future<Output = anyhow::Error>,
        check: impl FnOnce(&[u8]) -> bool,
        with_text: bool,
    ) -> Reply {
        self.sent += 1;
        let seq = self.sent;
        // Without a live connection the request includes lookup and
        // handshake; its time is not a round-trip time of the path.
        let new_conn = connection_to(&self.ep, self.to).is_none();
        let start = Instant::now();
        let result = tokio::select! {
            r = self.ep.request(self.to, payload) => r.map_err(anyhow::Error::from),
            e = cancel => Err(e),
        };
        let rtt = start.elapsed();
        let result = match result {
            Ok(r) if check(&r) => Ok(r),
            Ok(r) => Err(anyhow::anyhow!(
                "unexpected reply: {}",
                String::from_utf8_lossy(&r[..r.len().min(200)])
            )),
            Err(e) => Err(e),
        };
        let path = path_to(&self.ep, self.to);
        let mut ev = self.out.event("reply");
        if let Some(phase) = self.phase {
            ev = ev.field("phase", phase);
        }
        let mut ev = ev
            .field("seq", seq)
            .field("to", self.to.to_string())
            .field("bytes", payload.len())
            .field("path", path_name(path))
            .field("new_conn", new_conn);
        match &result {
            Ok(r) => {
                self.ok += 1;
                if !new_conn {
                    self.rtts.push(rtt);
                }
                if path == PathKind::Direct {
                    self.ok_direct += 1;
                    self.path_log.mark_direct();
                }
                self.connect.get_or_insert_with(|| self.t0.elapsed());
                ev = ev.field("ok", true).field("rtt_ms", ms(rtt));
                if with_text {
                    ev = ev.field("reply", String::from_utf8_lossy(r).into_owned());
                }
            }
            Err(e) => {
                ev = ev
                    .field("ok", false)
                    .field("rtt_ms", Value::Null)
                    .field("elapsed_ms", ms(rtt))
                    .field("error_kind", reply_error_kind(e))
                    .field("error", format!("{e:#}"));
            }
        }
        ev.emit();
        Reply {
            seq,
            path,
            rtt,
            result,
        }
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }

    pub fn ok(&self) -> u64 {
        self.ok
    }

    /// The results so far. `interrupted`: ended early by a signal.
    pub fn summary(&self, payload_size: usize, interrupted: bool) -> Summary {
        let first_direct = self
            .path_log
            .first_direct()
            .map(|t| ms(t.saturating_duration_since(self.t0)));
        Summary {
            to: self.to,
            sent: self.sent,
            ok: self.ok,
            failed: self.sent - self.ok,
            duration: self.t0.elapsed(),
            connect_ms: self.connect.map(ms),
            time_to_direct_ms: first_direct,
            direct_fraction: (self.ok > 0).then(|| round3(self.ok_direct as f64 / self.ok as f64)),
            rtt_ms: Stats::of(&self.rtts),
            rtt_samples: self.rtts.len(),
            jitter_ms: jitter(&self.rtts).map(ms),
            final_path: path_to(&self.ep, self.to),
            payload_size,
            interrupted,
        }
    }
}

/// Sends one request per `interval` for `duration` (requests are
/// sequential; missed intervals are skipped) until `stop` resolves. A request
/// still running at the end gets at most `timeout` more. Calls `on_reply`
/// after every request. Returns `true` if `stop` ended the series.
pub async fn ping_series<F: Future<Output = ()>>(
    pinger: &mut Pinger,
    duration: Duration,
    interval: Duration,
    payload_size: usize,
    timeout: Duration,
    stop: impl Fn() -> F,
    mut on_reply: impl FnMut(&Reply, &[u8]),
) -> bool {
    let end = tokio::time::Instant::now() + duration;
    // A request in flight at the end may take up to one more timeout.
    let hard_end = end + timeout;
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stop() => return true,
            _ = tick.tick() => {}
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        let payload = payload(pinger.sent() + 1, payload_size);
        let mut stopped = false;
        let cancel = async {
            tokio::select! {
                _ = tokio::time::sleep_until(hard_end) => {
                    anyhow::Error::from(golem_p2p::Error::Timeout)
                        .context("probe ended before the reply")
                }
                _ = stop() => {
                    stopped = true;
                    anyhow::anyhow!("interrupted")
                }
            }
        };
        let r = pinger
            .ping(&payload, cancel, |reply| reply.ends_with(&payload), false)
            .await;
        on_reply(&r, &payload);
        if stopped {
            return true;
        }
    }
}

/// Minimum, percentiles, maximum and mean of a set of durations, in
/// milliseconds; all null without samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Stats {
    pub min: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
}

impl Stats {
    pub fn of(samples: &[Duration]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort();
        let pct = |p: f64| percentile(&sorted, p).map(ms);
        Self {
            min: pct(0.0),
            p50: pct(50.0),
            p95: pct(95.0),
            max: pct(100.0),
            mean: (!sorted.is_empty())
                .then(|| ms(sorted.iter().sum::<Duration>() / sorted.len() as u32)),
        }
    }

    pub fn to_json(self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// Mean absolute difference between consecutive samples; `None` with fewer
/// than two.
pub fn jitter(samples: &[Duration]) -> Option<Duration> {
    if samples.len() < 2 {
        return None;
    }
    let sum: Duration = samples.windows(2).map(|w| w[0].abs_diff(w[1])).sum();
    Some(sum / (samples.len() - 1) as u32)
}

/// Results of a series of requests, see `docs/cli-events.md`.
pub struct Summary {
    pub to: NodeId,
    pub sent: u64,
    pub ok: u64,
    pub failed: u64,
    pub duration: Duration,
    pub connect_ms: Option<f64>,
    pub time_to_direct_ms: Option<f64>,
    pub direct_fraction: Option<f64>,
    pub rtt_ms: Stats,
    pub rtt_samples: usize,
    pub jitter_ms: Option<f64>,
    pub final_path: PathKind,
    pub payload_size: usize,
    pub interrupted: bool,
}

impl Summary {
    /// Emits the `summary` event; nothing is printed after it.
    pub fn emit(&self, out: &Out, bulk: Option<&Bulk>) {
        let ev = out
            .event("summary")
            .field("to", self.to.to_string())
            .field("sent", self.sent)
            .field("ok", self.ok)
            .field("failed", self.failed)
            .field("duration_ms", ms(self.duration))
            .field("connect_ms", self.connect_ms)
            .field("time_to_direct_ms", self.time_to_direct_ms)
            .field("direct_fraction", self.direct_fraction)
            .field("rtt_ms", self.rtt_ms.to_json())
            .field("rtt_samples", self.rtt_samples)
            .field("jitter_ms", self.jitter_ms)
            .field("final_path", path_name(self.final_path))
            .field("payload_size", self.payload_size)
            .field("interrupted", self.interrupted);
        let ev = match bulk {
            Some(b) => b.add_to(ev),
            None => ev
                .field("bulk_bytes", Value::Null)
                .field("bulk_ok", Value::Null)
                .field("bulk_ms", Value::Null)
                .field("throughput_mbps", Value::Null)
                .field("bulk_path", Value::Null),
        };
        ev.emit_last();
    }

    /// Human-readable summary lines.
    pub fn print_human(&self, bulk: Option<&Bulk>) {
        let f = |v: Option<f64>| v.map_or("-".to_string(), |v| v.to_string());
        let r = &self.rtt_ms;
        println!(
            "sent={} ok={} failed={} connect_ms={} time_to_direct_ms={} direct_fraction={} \
             rtt_ms min={} p50={} p95={} max={} jitter_ms={} final_path={}",
            self.sent,
            self.ok,
            self.failed,
            f(self.connect_ms),
            f(self.time_to_direct_ms),
            f(self.direct_fraction),
            f(r.min),
            f(r.p50),
            f(r.p95),
            f(r.max),
            f(self.jitter_ms),
            path_name(self.final_path),
        );
        if let Some(b) = bulk {
            match &b.result {
                Ok(r) => println!(
                    "bulk {} bytes in {} ms: {} Mbps ({})",
                    b.bytes,
                    ms(r.elapsed),
                    round3(r.mbps),
                    path_name(r.path)
                ),
                Err(e) => println!("bulk {} bytes failed: {e:#}", b.bytes),
            }
        }
    }
}

/// Nearest-rank percentile of sorted values; `p = 0` is the minimum.
fn percentile(sorted: &[Duration], p: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn reply_error_kind(e: &anyhow::Error) -> &'static str {
    if e.to_string().starts_with("unexpected reply") {
        return "mismatch";
    }
    anyhow_kind(e, "request")
}

/// Payload of probe request `seq`: an ASCII marker padded to `size` bytes
/// (longer than `size` only if the marker itself is).
pub fn payload(seq: u64, size: usize) -> Vec<u8> {
    let mut p = format!("probe seq={seq} ").into_bytes();
    if p.len() < size {
        p.resize(size, b'x');
    }
    p
}

/// The listener refused an upload or download because of one of its limits
/// (`listen --max-transfer-size`, `--max-transfers`, `--max-transfer-time`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", describe_refusal(.reason, .detail))]
pub struct Refused {
    /// `too_large`, `busy`, `timeout`, `bad_request`, ...
    pub reason: String,
    /// The rest of the reply, e.g. `max_bytes=1000`.
    pub detail: String,
}

impl Refused {
    /// Parses a `refused reason=... key=value` reply.
    pub fn parse(reply: &str) -> Option<Self> {
        let rest = reply.trim().strip_prefix("refused")?;
        let mut reason = String::new();
        let mut detail = Vec::new();
        for kv in rest.split_whitespace() {
            match kv.strip_prefix("reason=") {
                Some(r) => reason = r.to_string(),
                None => detail.push(kv),
            }
        }
        Some(Self {
            reason,
            detail: detail.join(" "),
        })
    }

    /// The value of `key=` in the detail.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.detail
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }
}

fn describe_refusal(reason: &str, detail: &str) -> String {
    let r = Refused {
        reason: reason.to_string(),
        detail: detail.to_string(),
    };
    match reason {
        "too_large" => format!(
            "the listener refused the transfer: it allows at most {} bytes per transfer",
            r.value("max_bytes").unwrap_or("?")
        ),
        "busy" => format!(
            "the listener refused the transfer: it already serves its maximum of {} transfers",
            r.value("max_transfers").unwrap_or("?")
        ),
        "timeout" => format!(
            "the listener stopped the transfer after its time limit of {} s",
            r.value("max_seconds").unwrap_or("?")
        ),
        _ => format!("the listener refused the transfer ({reason} {detail})"),
    }
}

/// The listener's count of an upload did not come back in time.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{}, but the listener did not report what it received within {} s",
    if *.stopped { "stopped the upload at the time limit" } else { "sent the whole upload" },
    .wait.as_secs()
)]
pub struct NoCount {
    /// How long the upload waited for the count.
    pub wait: Duration,
    /// The upload was stopped at its time limit (else everything was sent).
    pub stopped: bool,
}

/// The error kind of a failed transfer: `refused` for a listener limit,
/// `no_count` for an upload whose count did not come back, else the library
/// error kind, else `fallback`.
pub fn transfer_error_kind(e: &anyhow::Error, fallback: &'static str) -> &'static str {
    if e.chain().any(|c| c.is::<Refused>()) {
        "refused"
    } else if e.chain().any(|c| c.is::<NoCount>()) {
        "no_count"
    } else {
        anyhow_kind(e, fallback)
    }
}

/// A finished upload or download.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// Bytes transferred (confirmed by the listener for uploads).
    pub bytes: u64,
    pub elapsed: Duration,
    /// Megabits (10^6 bits) per second.
    pub mbps: f64,
    /// Path at the end of the transfer.
    pub path: PathKind,
    /// Ended by the time limit before all bytes were transferred.
    pub by_time: bool,
}

/// After an upload was stopped at its time limit, how long to wait at least
/// for the listener's count of what arrived (see [`count_wait`]).
pub const COUNT_WAIT: Duration = Duration::from_secs(3);

/// The longest wait for the listener's count of a stopped upload.
pub const COUNT_WAIT_MAX: Duration = Duration::from_secs(30);

/// How long to wait for the listener's count after stopping an upload that
/// ran for `upload_time`, on a path with round-trip time `rtt` (QUIC's
/// smoothed RTT, which includes queueing): at least [`COUNT_WAIT`], as long
/// as the upload ran, or three round trips, whichever is longest (at most
/// [`COUNT_WAIT_MAX`]). The stop (a stream reset) reaches the listener only
/// after what is still queued on the path: on a slow path that can take
/// seconds (campaign 2: a relayed upload over a lossy link, stopped at 8 s,
/// was counted 4-5 s later).
pub fn count_wait(upload_time: Duration, rtt: Option<Duration>) -> Duration {
    let rtts = rtt.unwrap_or_default().saturating_mul(3);
    COUNT_WAIT.max(upload_time).max(rtts).min(COUNT_WAIT_MAX)
}

/// Megabits (10^6 bits) per second.
pub fn mbps(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 * 8.0 / elapsed.as_secs_f64().max(1e-9) / 1e6
}

/// Uploads up to `max_bytes` to the listener's sink in one stream and waits
/// for its count. With `max_time`, writing ends then: whatever is not yet
/// delivered is dropped (the stream is reset with [`UPLOAD_STOPPED`]), and
/// the listener reports what arrived until then (waiting at most
/// [`count_wait`] more for that, else [`NoCount`]). An upload written
/// completely before `max_time` is not stopped: what the stream buffered is
/// still on its way, and the listener counts it once it has arrived, which
/// on a slow path can be after `max_time`. Its count is awaited until
/// `max_time`, or [`count_wait`] after the write if that is later.
pub async fn upload(
    ep: &Endpoint,
    to: NodeId,
    max_bytes: u64,
    max_time: Option<Duration>,
) -> anyhow::Result<Transfer> {
    let start = Instant::now();
    let deadline = max_time.map(|t| tokio::time::Instant::from_std(start + t));
    let (mut send, mut recv) = ep.open_bi(to).await?;
    let mut sent = 0u64;
    let write = async {
        send.write_all(SINK_MAGIC).await?;
        let chunk = vec![0u8; 64 * 1024];
        while sent < max_bytes {
            let n = (max_bytes - sent).min(chunk.len() as u64) as usize;
            // `write`, not `write_all`: cut off by the time limit, `sent`
            // must still be exactly what the stream took.
            sent += send.write(&chunk[..n]).await? as u64;
        }
        send.finish()?;
        Ok::<_, anyhow::Error>(())
    };
    // `None`: the time limit cut writing off.
    let written = match deadline {
        Some(d) => tokio::time::timeout_at(d, write).await.ok(),
        None => Some(write.await),
    };
    let by_time = written.is_none();
    // The listener's count is read once: `read_to_end` cut off by a timeout
    // leaves the stream unreadable ("closed stream").
    let reply = match written {
        // Everything written: wait for the count, past the time limit if
        // need be (see above).
        Some(Ok(())) => match deadline {
            Some(d) => {
                let now = tokio::time::Instant::now();
                let until = d.max(now + count_wait(start.elapsed(), path_rtt(ep, to)));
                match tokio::time::timeout_at(until, recv.read_to_end(4096)).await {
                    Ok(r) => r?,
                    Err(_) => {
                        return Err(NoCount {
                            wait: until - now,
                            stopped: false,
                        }
                        .into());
                    }
                }
            }
            None => recv.read_to_end(4096).await?,
        },
        // Read the reply also if writing failed: a refusal says why.
        Some(Err(e)) => {
            match tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(4096)).await {
                Ok(Ok(r)) if r.starts_with(b"refused") => r,
                _ => return Err(e),
            }
        }
        // Cut off by the time limit: stop; the listener counts what arrived.
        None => {
            let _ = send.reset(UPLOAD_STOPPED);
            let wait = count_wait(start.elapsed(), path_rtt(ep, to));
            match tokio::time::timeout(wait, recv.read_to_end(4096)).await {
                Ok(r) => r?,
                Err(_) => {
                    return Err(NoCount {
                        wait,
                        stopped: true,
                    }
                    .into());
                }
            }
        }
    };
    let elapsed = start.elapsed();
    let reply = String::from_utf8_lossy(&reply);
    if let Some(r) = Refused::parse(&reply) {
        return Err(r.into());
    }
    let got: u64 = reply
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("sink_bytes="))
        .with_context(|| {
            if by_time {
                "the listener did not report what it received after the upload was stopped at \
                 the time limit (an older golem-p2p listener?)"
            } else {
                "listener does not support bulk transfers"
            }
        })?
        .parse()
        .context("invalid sink reply")?;
    // Stopped early, the listener got what arrived before the reset.
    if got > sent || (!by_time && got != sent) {
        bail!("listener received {got} of {sent} bytes");
    }
    Ok(Transfer {
        bytes: got,
        elapsed,
        mbps: mbps(got, elapsed),
        path: path_to(ep, to),
        by_time,
    })
}

/// Downloads up to `max_bytes` from the listener's source in one stream.
/// With `max_time`, stops reading after that long (also if the download has
/// not started by then: an error).
pub async fn download(
    ep: &Endpoint,
    to: NodeId,
    max_bytes: u64,
    max_time: Option<Duration>,
) -> anyhow::Result<Transfer> {
    let start = Instant::now();
    let deadline = max_time.map(|t| tokio::time::Instant::from_std(start + t));
    let (mut send, mut recv) = ep.open_bi(to).await?;
    let mut req = SOURCE_MAGIC.to_vec();
    req.extend_from_slice(format!("{max_bytes}\n").as_bytes());
    send.write_all(&req).await?;
    send.finish()?;
    let (status, rest) = match deadline {
        Some(d) => tokio::time::timeout_at(d, read_status_line(&mut recv))
            .await
            .map_err(|_| {
                anyhow::Error::from(golem_p2p::Error::Timeout)
                    .context("the download did not start within the time limit")
            })??,
        None => read_status_line(&mut recv).await?,
    };
    if let Some(r) = Refused::parse(&status) {
        return Err(r.into());
    }
    let announced: u64 = status
        .strip_prefix("ok bytes=")
        .and_then(|n| n.trim().parse().ok())
        .with_context(|| {
            format!(
                "listener does not support downloads (an older golem-p2p?): {}",
                &status[..status.len().min(100)]
            )
        })?;
    let mut got = rest.len() as u64;
    let mut by_time = false;
    // `read_chunk` is cancel-safe: cut off by the time limit, nothing read
    // is lost (and the stream is not read again).
    loop {
        let chunk = match deadline {
            Some(d) => match tokio::time::timeout_at(d, recv.read_chunk(usize::MAX)).await {
                Ok(c) => c,
                Err(_) => {
                    by_time = true;
                    let _ = recv.stop(golem_p2p::iroh::endpoint::VarInt::from_u32(0));
                    break;
                }
            },
            None => recv.read_chunk(usize::MAX).await,
        };
        match chunk {
            Ok(Some(c)) => got += c.len() as u64,
            Ok(None) => break,
            Err(golem_p2p::iroh::endpoint::ReadError::Reset(code)) if code == REFUSED => {
                return Err(Refused {
                    reason: "timeout".into(),
                    detail: String::new(),
                }
                .into());
            }
            Err(e) => return Err(e.into()),
        }
    }
    let elapsed = start.elapsed();
    // Every byte arrived, only the end of the stream not yet: complete.
    by_time &= got < announced;
    if !by_time && got != announced {
        bail!("listener sent {got} of {announced} bytes");
    }
    Ok(Transfer {
        bytes: got,
        elapsed,
        mbps: mbps(got, elapsed),
        path: path_to(ep, to),
        by_time,
    })
}

/// Reads the status line of a download reply: everything up to the first
/// `\n` (at most 256 bytes). Returns the line and the bytes after it.
async fn read_status_line(recv: &mut RecvStream) -> anyhow::Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    loop {
        if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let rest = buf.split_off(i + 1);
            buf.truncate(i);
            return Ok((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        if buf.len() > 256 {
            break;
        }
        match recv.read_chunk(usize::MAX).await? {
            Some(c) => buf.extend_from_slice(&c),
            None => break,
        }
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), Vec::new()))
}

/// A bulk upload of `probe --bulk`.
pub struct Bulk {
    pub bytes: u64,
    pub result: anyhow::Result<Transfer>,
}

impl Bulk {
    /// Sends `bytes` bytes in one stream and waits for the sink's count.
    pub async fn run(ep: &Endpoint, to: NodeId, bytes: u64, timeout: Duration) -> Self {
        let result = tokio::time::timeout(timeout, upload(ep, to, bytes, None))
            .await
            .unwrap_or_else(|_| Err(anyhow::Error::from(golem_p2p::Error::Timeout)));
        Self { bytes, result }
    }

    fn add_to<'a>(&self, ev: crate::out::Ev<'a>) -> crate::out::Ev<'a> {
        let ev = ev.field("bulk_bytes", self.bytes);
        match &self.result {
            Ok(r) => ev
                .field("bulk_ok", true)
                .field("bulk_ms", ms(r.elapsed))
                .field("throughput_mbps", round3(r.mbps))
                .field("bulk_path", path_name(r.path)),
            Err(_) => ev
                .field("bulk_ok", false)
                .field("bulk_ms", Value::Null)
                .field("throughput_mbps", Value::Null)
                .field("bulk_path", Value::Null),
        }
    }

    /// Emits an `error` event if the transfer failed.
    pub fn report_error(&self, out: &Out) {
        if let Err(e) = &self.result {
            out.event("error")
                .field("kind", "bulk")
                .field("cause", transfer_error_kind(e, "bulk"))
                .field(
                    "message",
                    format!("bulk transfer of {} bytes: {e:#}", self.bytes),
                )
                .emit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v: Vec<Duration> = (1..=20).map(Duration::from_millis).collect();
        let p = |x| percentile(&v, x).unwrap().as_millis();
        assert_eq!(p(0.0), 1);
        assert_eq!(p(50.0), 10);
        assert_eq!(p(95.0), 19);
        assert_eq!(p(100.0), 20);
        assert_eq!(percentile(&v[..1], 95.0), Some(Duration::from_millis(1)));
        assert_eq!(percentile(&[], 50.0), None);
        let s = Stats::of(&v);
        assert_eq!((s.min, s.p50, s.max), (Some(1.0), Some(10.0), Some(20.0)));
        assert_eq!(s.mean, Some(10.5));
        assert_eq!(Stats::of(&[]), Stats::default());
    }

    #[test]
    fn jitters() {
        let d = |v: &[u64]| {
            v.iter()
                .map(|x| Duration::from_millis(*x))
                .collect::<Vec<_>>()
        };
        assert_eq!(jitter(&d(&[10])), None);
        assert_eq!(jitter(&d(&[10, 10, 10])), Some(Duration::ZERO));
        // |20-10| + |15-20| + |15-15| = 15, over 3 differences.
        assert_eq!(
            jitter(&d(&[10, 20, 15, 15])),
            Some(Duration::from_millis(5))
        );
    }

    #[test]
    fn payloads() {
        assert_eq!(payload(7, 0), b"probe seq=7 ");
        assert_eq!(payload(7, 16), b"probe seq=7 xxxx");
        assert_eq!(payload(12, 1000).len(), 1000);
    }

    #[test]
    fn throughput() {
        let v = mbps(1_000_000, Duration::from_secs(1));
        assert!((v - 8.0).abs() < 1e-9);
    }

    #[test]
    fn refusals() {
        let r = Refused::parse("refused reason=too_large max_bytes=1000\n").unwrap();
        assert_eq!(r.reason, "too_large");
        assert_eq!(r.value("max_bytes"), Some("1000"));
        assert!(r.to_string().contains("at most 1000 bytes"), "{r}");
        let r = Refused::parse("refused reason=busy max_transfers=0").unwrap();
        assert!(r.to_string().contains("maximum of 0 transfers"), "{r}");
        assert_eq!(Refused::parse("endpoint=x sink_bytes=3"), None);
        let e = anyhow::Error::from(r).context("upload");
        assert_eq!(transfer_error_kind(&e, "bulk"), "refused");
        let e = anyhow::Error::from(golem_p2p::Error::Timeout);
        assert_eq!(transfer_error_kind(&e, "bulk"), "timeout");
        let e = anyhow::Error::from(NoCount {
            wait: Duration::from_secs(8),
            stopped: true,
        });
        assert_eq!(transfer_error_kind(&e, "transfer"), "no_count");
        assert!(e.to_string().starts_with("stopped the upload"), "{e}");
        assert!(e.to_string().ends_with("within 8 s"), "{e}");
        let e = anyhow::Error::from(NoCount {
            wait: Duration::from_secs(3),
            stopped: false,
        });
        assert_eq!(transfer_error_kind(&e, "transfer"), "no_count");
        assert!(e.to_string().starts_with("sent the whole upload"), "{e}");
    }

    #[test]
    fn count_waits() {
        let s = Duration::from_secs;
        let ms = Duration::from_millis;
        // A short upload on a fast path: at least 3 s.
        assert_eq!(count_wait(ms(200), Some(ms(1))), s(3));
        assert_eq!(count_wait(ms(200), None), s(3));
        // Stopped at 8 s (--transfer-time): as long again, also on a fast
        // path whose RTT says nothing about what is queued.
        assert_eq!(count_wait(s(8), Some(ms(160))), s(8));
        // Seconds of queueing show in the RTT: three round trips.
        assert_eq!(count_wait(s(8), Some(s(4))), s(12));
        // Never more than 30 s.
        assert_eq!(count_wait(s(8), Some(s(60))), COUNT_WAIT_MAX);
        assert_eq!(count_wait(s(100), None), COUNT_WAIT_MAX);
    }

    #[test]
    fn error_kinds() {
        let e = anyhow::Error::from(golem_p2p::Error::Timeout).context("probe ended");
        assert_eq!(reply_error_kind(&e), "timeout");
        assert_eq!(
            reply_error_kind(&anyhow::anyhow!("unexpected reply: x")),
            "mismatch"
        );
        assert_eq!(crate::out::error_kind(&golem_p2p::Error::Closed), "closed");
    }

    /// Two endpoints on loopback, direct only, connected.
    async fn pair() -> (Endpoint, Endpoint) {
        use golem_p2p::{Identities, InMemorySigner, MemoryDiscovery, TransportMode};
        let discovery = Arc::new(MemoryDiscovery::new());
        let mut eps = Vec::new();
        for _ in 0..2 {
            let signer = InMemorySigner::new();
            let id = signer.generate();
            let ep = Endpoint::builder(signer, Identities::new(id))
                .discovery(discovery.clone())
                .transport_mode(TransportMode::DirectOnly)
                .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0).into())
                .bind()
                .await
                .unwrap();
            eps.push(ep);
        }
        let (b, a) = (eps.pop().unwrap(), eps.pop().unwrap());
        a.connect(b.node_id()).await.unwrap();
        (a, b)
    }

    /// Accepts one upload on `ep`, reads it as the listener does, waits
    /// `delay`, then replies with the count. `pace`: read in chunks of at
    /// most 16 KiB with this pause after each (a slow link).
    fn sink(
        ep: &Endpoint,
        delay: Duration,
        pace: Option<Duration>,
    ) -> tokio::task::JoinHandle<u64> {
        let ep = ep.clone();
        tokio::spawn(async move {
            let mut inc = ep.accept().await.unwrap();
            let mut n = 0u64;
            {
                let (_, recv) = inc.streams();
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match recv.read(&mut buf).await {
                        Ok(Some(k)) => n += k as u64,
                        Ok(None) => break,
                        Err(golem_p2p::iroh::endpoint::ReadError::Reset(c))
                            if c == UPLOAD_STOPPED =>
                        {
                            break;
                        }
                        Err(e) => panic!("sink: {e}"),
                    }
                    if let Some(p) = pace {
                        tokio::time::sleep(p).await;
                    }
                }
            }
            n -= SINK_MAGIC.len() as u64;
            tokio::time::sleep(delay).await;
            inc.respond(format!("sink_bytes={n}").as_bytes())
                .await
                .unwrap();
            n
        })
    }

    /// An upload written completely before the time limit is complete, also
    /// if the listener's count comes back only after the limit (campaign 3,
    /// debug/lossy-extra run 10: the listener had all 10 MB at 7.9 s, the
    /// count arrived after 8 s, and the upload failed with "read error:
    /// closed stream").
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upload_counted_after_the_time_limit() {
        let (a, b) = pair().await;
        let counted = sink(&b, Duration::from_millis(600), None);
        let limit = Duration::from_millis(200);
        let t = upload(&a, b.node_id(), 100_000, Some(limit)).await.unwrap();
        assert_eq!(counted.await.unwrap(), 100_000);
        assert_eq!(t.bytes, 100_000);
        assert!(!t.by_time, "{t:?}");
        assert!(t.elapsed >= Duration::from_millis(600), "{t:?}");
        a.close().await;
        b.close().await;
    }

    /// An upload cut off by the time limit while writing is stopped, and
    /// reports what the listener received until then.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upload_stopped_at_the_time_limit() {
        let (a, b) = pair().await;
        let counted = sink(&b, Duration::ZERO, Some(Duration::from_millis(20)));
        let t = upload(
            &a,
            b.node_id(),
            50_000_000,
            Some(Duration::from_millis(300)),
        )
        .await
        .unwrap();
        let n = counted.await.unwrap();
        assert!(t.by_time, "{t:?}");
        assert_eq!(t.bytes, n);
        assert!(n > 0 && n < 50_000_000, "{t:?}");
        a.close().await;
        b.close().await;
    }
}
