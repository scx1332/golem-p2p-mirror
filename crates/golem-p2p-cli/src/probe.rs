//! Request/reply measurement shared by `send` and `probe`, and the bulk
//! transfer of `probe --bulk`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use golem_p2p::{Endpoint, NodeId, PathKind};
use serde_json::{Value, json};

use crate::{
    listen::SINK_MAGIC,
    observe::{FirstDirect, connection_to, path_to},
    out::{Out, anyhow_kind, ms, path_name, round3},
};

/// Counters and timings of a series of requests to one peer.
pub struct Pinger {
    ep: Endpoint,
    to: NodeId,
    out: Out,
    /// When the first request was started. All `*_ms` times count from here.
    t0: Instant,
    first_direct: Arc<FirstDirect>,
    sent: u64,
    ok: u64,
    ok_direct: u64,
    /// Round-trip times of successful requests on an existing connection.
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
    pub fn new(ep: &Endpoint, to: NodeId, out: &Out, first_direct: Arc<FirstDirect>) -> Self {
        Self {
            ep: ep.clone(),
            to,
            out: out.clone(),
            t0: Instant::now(),
            first_direct,
            sent: 0,
            ok: 0,
            ok_direct: 0,
            rtts: Vec::new(),
            connect: None,
        }
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
        let mut ev = self
            .out
            .event("reply")
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
                    self.first_direct.mark();
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
        let mut sorted = self.rtts.clone();
        sorted.sort();
        let pct = |p: f64| percentile(&sorted, p).map(ms);
        let mean =
            (!sorted.is_empty()).then(|| ms(sorted.iter().sum::<Duration>() / sorted.len() as u32));
        let first_direct = self
            .first_direct
            .get()
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
            rtt_ms: json!({
                "min": pct(0.0),
                "p50": pct(50.0),
                "p95": pct(95.0),
                "max": pct(100.0),
                "mean": mean,
            }),
            rtt_samples: sorted.len(),
            final_path: path_to(&self.ep, self.to),
            payload_size,
            interrupted,
        }
    }
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
    pub rtt_ms: Value,
    pub rtt_samples: usize,
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
            .field("rtt_ms", self.rtt_ms.clone())
            .field("rtt_samples", self.rtt_samples)
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
        let r = |k: &str| self.rtt_ms[k].as_f64();
        println!(
            "sent={} ok={} failed={} connect_ms={} time_to_direct_ms={} direct_fraction={} \
             rtt_ms min={} p50={} p95={} max={} final_path={}",
            self.sent,
            self.ok,
            self.failed,
            f(self.connect_ms),
            f(self.time_to_direct_ms),
            f(self.direct_fraction),
            f(r("min")),
            f(r("p50")),
            f(r("p95")),
            f(r("max")),
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

/// A bulk transfer to the listener's sink.
pub struct Bulk {
    pub bytes: u64,
    pub result: anyhow::Result<BulkResult>,
}

pub struct BulkResult {
    pub elapsed: Duration,
    /// Megabits (10^6 bits) per second.
    pub mbps: f64,
    pub path: PathKind,
}

/// Megabits (10^6 bits) per second.
fn mbps(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 * 8.0 / elapsed.as_secs_f64().max(1e-9) / 1e6
}

impl Bulk {
    /// Sends `bytes` bytes in one stream and waits for the sink's count.
    pub async fn run(ep: &Endpoint, to: NodeId, bytes: u64, timeout: Duration) -> Self {
        let result = tokio::time::timeout(timeout, transfer(ep, to, bytes))
            .await
            .unwrap_or_else(|_| Err(anyhow::Error::from(golem_p2p::Error::Timeout)))
            .map(|elapsed| BulkResult {
                elapsed,
                mbps: mbps(bytes, elapsed),
                path: path_to(ep, to),
            });
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

async fn transfer(ep: &Endpoint, to: NodeId, bytes: u64) -> anyhow::Result<Duration> {
    let start = Instant::now();
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
    }
    .await;
    // Read the reply also if writing failed: a refusal says why.
    let reply = match write {
        Ok(()) => recv.read_to_end(4096).await?,
        Err(e) => {
            match tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(4096)).await {
                Ok(Ok(r)) if r.starts_with(b"refused") => r,
                _ => return Err(e),
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
        .context("listener does not support bulk transfers")?
        .parse()
        .context("invalid sink reply")?;
    if got != bytes {
        bail!("listener received {got} of {bytes} bytes");
    }
    Ok(elapsed)
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

/// The error kind of a failed transfer: `refused` for a listener limit,
/// else the library error kind, else `fallback`.
pub fn transfer_error_kind(e: &anyhow::Error, fallback: &'static str) -> &'static str {
    if e.chain().any(|c| c.is::<Refused>()) {
        "refused"
    } else {
        anyhow_kind(e, fallback)
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
}
