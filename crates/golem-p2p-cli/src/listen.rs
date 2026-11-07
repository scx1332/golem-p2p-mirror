//! The `listen` side: answers requests with an echo, swallows uploads (sink)
//! and serves downloads (source), within server-side limits.
//!
//! Every application stream is one request:
//!
//! * [`SINK_MAGIC`] followed by any number of bytes: an upload (`probe
//!   --bulk`, `bench`). The bytes are counted and discarded; the reply reports
//!   the count.
//! * [`SOURCE_MAGIC`] followed by a decimal byte count: a download (`bench`).
//!   The reply is a status line and then that many bytes.
//! * Anything else is an echo request of at most [`MAX_REQUEST`] bytes.
//!
//! Uploads and downloads are bounded by [`Limits`]: bytes per stream, streams
//! at once, and time per stream. A request beyond a limit is refused with a
//! `refused reason=...` reply (see `docs/cli-events.md`).

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use golem_p2p::{
    Endpoint, IncomingStream, NodeId, SendStream,
    iroh::endpoint::{ReadError, VarInt},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::debug;

use crate::{
    observe::SelectedPath,
    out::{Out, ms, path_name},
};

/// First bytes of an upload stream.
pub const SINK_MAGIC: &[u8] = b"golem-p2p-cli/sink/1\n";

/// First bytes of a download request.
pub const SOURCE_MAGIC: &[u8] = b"golem-p2p-cli/source/1\n";

/// Maximum size of an echo request.
pub const MAX_REQUEST: usize = 1 << 20;

/// Stream error code with which the listener stops an upload or resets a
/// download that exceeds a limit.
pub const REFUSED: VarInt = VarInt::from_u32(0x100);

/// Stream error code with which an uploader resets an upload that it stops
/// at its time limit (`bench --transfer-time`). The listener ends the upload
/// there and still replies with the bytes it received.
pub const UPLOAD_STOPPED: VarInt = VarInt::from_u32(0x101);

/// Longest time to receive an echo request, or the start of an upload or a
/// download request, before the stream is refused. Without it a peer could
/// keep streams (and up to [`MAX_REQUEST`] bytes each) open indefinitely.
pub const REQUEST_TIME: Duration = Duration::from_secs(10);

/// Maximum size of a download request (magic plus byte count).
const MAX_SOURCE_REQUEST: usize = 64;

/// Size of the chunks a download is written in.
const CHUNK: usize = 64 * 1024;

/// Server-side limits for uploads and downloads.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest upload or download per stream, in bytes.
    pub max_transfer_size: u64,
    /// Uploads and downloads served at once, over all peers.
    pub max_transfers: usize,
    /// Longest time one upload or download may take.
    pub max_transfer_time: Duration,
}

/// Serves requests until the endpoint is closed.
pub async fn serve(ep: Endpoint, out: Out, limits: Limits) {
    let endpoint_id = ep.endpoint_id().to_string();
    let instance = hex::encode(ep.instance_id());
    let slots = Arc::new(Semaphore::new(limits.max_transfers));
    while let Some(inc) = ep.accept().await {
        let out = out.clone();
        let me = format!("endpoint={endpoint_id} instance={instance}");
        let slots = slots.clone();
        tokio::spawn(async move {
            let from = inc.from();
            if let Err(e) = handle(inc, &out, &me, &limits, &slots).await {
                debug!(%from, "request failed: {e}");
                out.event("error")
                    .field("kind", crate::out::anyhow_kind(&e, "request"))
                    .field("message", format!("request from {from}: {e:#}"))
                    .field("peer", from.to_string())
                    .emit();
            }
        });
    }
}

async fn handle(
    mut inc: IncomingStream,
    out: &Out,
    me: &str,
    limits: &Limits,
    slots: &Arc<Semaphore>,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + REQUEST_TIME;
    let (_, recv) = inc.streams();
    // Read just enough to tell an upload or download from an echo request.
    let mut head = vec![0u8; SINK_MAGIC.len().max(SOURCE_MAGIC.len())];
    let mut filled = 0;
    let mut finished = false;
    let read_head = async {
        while filled < head.len() {
            match recv.read(&mut head[filled..]).await? {
                Some(n) => filled += n,
                None => {
                    finished = true;
                    break;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    match tokio::time::timeout_at(deadline, read_head).await {
        Ok(r) => r?,
        Err(_) => return Err(too_slow(&mut inc)),
    }
    head.truncate(filled);

    if head.starts_with(SINK_MAGIC) {
        let already = (head.len() - SINK_MAGIC.len()) as u64;
        return sink(inc, out, me, limits, slots, start, already).await;
    }
    if head.starts_with(SOURCE_MAGIC) {
        let mut req = head.split_off(SOURCE_MAGIC.len());
        if !finished {
            let (_, recv) = inc.streams();
            let max = MAX_SOURCE_REQUEST.saturating_sub(req.len());
            // A request that is not finished in time is malformed.
            match tokio::time::timeout(REQUEST_TIME, recv.read_to_end(max)).await {
                Ok(rest) => req.extend_from_slice(&rest?),
                Err(_) => req.clear(),
            }
        }
        return source(inc, out, limits, slots, start, &req).await;
    }

    if !finished {
        let (_, recv) = inc.streams();
        let read = recv.read_to_end(MAX_REQUEST - head.len());
        match tokio::time::timeout_at(deadline, read).await {
            Ok(rest) => head.extend_from_slice(&rest?),
            Err(_) => return Err(too_slow(&mut inc)),
        }
    }
    let req = head;
    let (from, to) = (inc.from(), inc.to());
    let path = path_name(SelectedPath::of(inc.connection()).kind);
    if out.json() {
        out.event("request")
            .field("from", from.to_string())
            .field("to", to.to_string())
            .field("path", path)
            .field("bytes", req.len())
            .field("kind", "echo")
            .emit();
    } else {
        eprintln!("request from {from} to {to} via {path}: {}", preview(&req));
    }
    let reply = format!(
        "{me} to={to} from={from} echo={}",
        String::from_utf8_lossy(&req)
    );
    inc.respond(reply.as_bytes()).await?;
    Ok(())
}

/// Refuses a request that did not arrive within [`REQUEST_TIME`].
fn too_slow(inc: &mut IncomingStream) -> anyhow::Error {
    let (send, recv) = inc.streams();
    let _ = recv.stop(REFUSED);
    let _ = send.reset(REFUSED);
    anyhow::Error::from(golem_p2p::Error::Timeout).context(format!(
        "request not received within {} s",
        REQUEST_TIME.as_secs()
    ))
}

/// The start of a request for human output: long requests are cut.
fn preview(req: &[u8]) -> String {
    const MAX: usize = 100;
    if req.len() <= MAX {
        String::from_utf8_lossy(req).into_owned()
    } else {
        format!(
            "{}... ({} bytes)",
            String::from_utf8_lossy(&req[..MAX]),
            req.len()
        )
    }
}

/// Why an upload or download was refused, as sent in the reply.
fn refusal(reason: &str, limits: &Limits) -> String {
    match reason {
        "too_large" => format!(
            "refused reason=too_large max_bytes={}",
            limits.max_transfer_size
        ),
        "busy" => format!("refused reason=busy max_transfers={}", limits.max_transfers),
        "timeout" => format!(
            "refused reason=timeout max_seconds={}",
            limits.max_transfer_time.as_secs_f64()
        ),
        other => format!("refused reason={other}"),
    }
}

fn try_slot(slots: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    slots.clone().try_acquire_owned().ok()
}

/// One finished (or refused) upload or download.
struct TransferLog {
    kind: &'static str,
    inc_from: NodeId,
    inc_to: NodeId,
    path: &'static str,
    bytes: u64,
    requested: Option<u64>,
    elapsed: Duration,
    refused: Option<&'static str>,
}

impl TransferLog {
    fn emit(&self, out: &Out) {
        if out.json() {
            let mut ev = out
                .event("request")
                .field("from", self.inc_from.to_string())
                .field("to", self.inc_to.to_string())
                .field("path", self.path)
                .field("bytes", self.bytes)
                .field("kind", self.kind)
                .field("ms", ms(self.elapsed));
            if self.kind == "source" {
                ev = ev.field("requested", self.requested);
            }
            ev.field("refused", self.refused).emit();
        } else {
            let what = match self.kind {
                "sink" => "upload",
                _ => "download",
            };
            let refused = self
                .refused
                .map(|r| format!(", refused: {r}"))
                .unwrap_or_default();
            eprintln!(
                "{what} {} {} via {}: {} bytes in {} ms{refused}",
                if self.kind == "sink" { "from" } else { "to" },
                self.inc_from,
                self.path,
                self.bytes,
                self.elapsed.as_millis()
            );
        }
    }
}

/// Counts and discards an upload; `already` bytes were read with the magic.
async fn sink(
    mut inc: IncomingStream,
    out: &Out,
    me: &str,
    limits: &Limits,
    slots: &Arc<Semaphore>,
    start: Instant,
    already: u64,
) -> anyhow::Result<()> {
    let (from, to) = (inc.from(), inc.to());
    let mut n = already;
    let refused = match try_slot(slots) {
        None => Some("busy"),
        Some(_permit) => {
            let (_, recv) = inc.streams();
            let read = async {
                if n > limits.max_transfer_size {
                    return Ok(Some("too_large"));
                }
                loop {
                    match recv.read_chunk(usize::MAX).await {
                        Ok(Some(chunk)) => n += chunk.len() as u64,
                        Ok(None) => break,
                        // Stopped by the uploader at its time limit: count
                        // what arrived.
                        Err(ReadError::Reset(code)) if code == UPLOAD_STOPPED => break,
                        Err(e) => return Err(e.into()),
                    }
                    if n > limits.max_transfer_size {
                        return Ok(Some("too_large"));
                    }
                }
                Ok::<_, anyhow::Error>(None)
            };
            match tokio::time::timeout(limits.max_transfer_time, read).await {
                Ok(r) => r?,
                Err(_) => Some("timeout"),
            }
        }
    };
    let log = TransferLog {
        kind: "sink",
        inc_from: from,
        inc_to: to,
        path: path_name(SelectedPath::of(inc.connection()).kind),
        bytes: n,
        requested: None,
        elapsed: start.elapsed(),
        refused,
    };
    log.emit(out);
    let reply = match refused {
        Some(reason) => {
            // Stop the upload at once; the uploader reads the reason.
            let (_, recv) = inc.streams();
            let _ = recv.stop(REFUSED);
            refusal(reason, limits)
        }
        None => format!("{me} to={to} from={from} sink_bytes={n}"),
    };
    inc.respond(reply.as_bytes()).await?;
    Ok(())
}

/// Serves a download: `ok bytes=N` and N bytes, or a refusal.
async fn source(
    inc: IncomingStream,
    out: &Out,
    limits: &Limits,
    slots: &Arc<Semaphore>,
    start: Instant,
    req: &[u8],
) -> anyhow::Result<()> {
    let (from, to) = (inc.from(), inc.to());
    let path = path_name(SelectedPath::of(inc.connection()).kind);
    let requested: Option<u64> = std::str::from_utf8(req)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let mut log = TransferLog {
        kind: "source",
        inc_from: from,
        inc_to: to,
        path,
        bytes: 0,
        requested,
        elapsed: Duration::ZERO,
        refused: None,
    };
    let permit = try_slot(slots);
    let refused = match (requested, &permit) {
        (None, _) => Some("bad_request"),
        (Some(n), _) if n > limits.max_transfer_size => Some("too_large"),
        (_, None) => Some("busy"),
        _ => None,
    };
    if let Some(reason) = refused {
        log.refused = Some(reason);
        log.elapsed = start.elapsed();
        log.emit(out);
        let reply = refusal(reason, limits) + "\n";
        inc.respond(reply.as_bytes()).await?;
        return Ok(());
    }
    let requested = requested.unwrap_or_default();
    let (mut send, _recv) = inc.into_streams();
    let mut sent = 0u64;
    let write = write_source(&mut send, requested, &mut sent);
    let result = tokio::time::timeout(limits.max_transfer_time, write).await;
    drop(permit);
    log.bytes = sent;
    log.elapsed = start.elapsed();
    match result {
        Ok(Ok(())) => {
            // Wait until the peer has read everything (or stopped reading).
            let _ = tokio::time::timeout(Duration::from_secs(10), send.stopped()).await;
        }
        Ok(Err(e)) => {
            // The downloader may stop reading early (its time limit).
            debug!(%from, "download ended early after {sent} bytes: {e}");
        }
        Err(_) => {
            let _ = send.reset(REFUSED);
            log.refused = Some("timeout");
        }
    }
    log.emit(out);
    Ok(())
}

async fn write_source(send: &mut SendStream, bytes: u64, sent: &mut u64) -> anyhow::Result<()> {
    send.write_all(format!("ok bytes={bytes}\n").as_bytes())
        .await?;
    let chunk = vec![0u8; CHUNK];
    while *sent < bytes {
        let n = (bytes - *sent).min(CHUNK as u64) as usize;
        send.write_all(&chunk[..n]).await?;
        *sent += n as u64;
    }
    send.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals() {
        let limits = Limits {
            max_transfer_size: 1000,
            max_transfers: 2,
            max_transfer_time: Duration::from_millis(1500),
        };
        assert_eq!(
            refusal("too_large", &limits),
            "refused reason=too_large max_bytes=1000"
        );
        assert_eq!(
            refusal("busy", &limits),
            "refused reason=busy max_transfers=2"
        );
        assert_eq!(
            refusal("timeout", &limits),
            "refused reason=timeout max_seconds=1.5"
        );
        assert_eq!(
            refusal("bad_request", &limits),
            "refused reason=bad_request"
        );
    }

    #[test]
    fn previews() {
        assert_eq!(preview(b"hi"), "hi");
        let long = vec![b'x'; 300];
        let p = preview(&long);
        assert!(p.ends_with("... (300 bytes)"), "{p}");
        assert_eq!(p.len(), 100 + "... (300 bytes)".len());
    }
}
