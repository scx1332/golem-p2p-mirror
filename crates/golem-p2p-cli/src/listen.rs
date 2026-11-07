//! The `listen` side: answers requests with an echo and swallows bulk
//! transfers.
//!
//! Every application stream is one request. If it starts with
//! [`SINK_MAGIC`], everything after the magic is read and discarded, and the
//! reply reports the byte count (`probe --bulk`). Anything else is an echo
//! request of at most [`MAX_REQUEST`] bytes.

use std::time::Instant;

use golem_p2p::{Endpoint, IncomingStream};
use tracing::debug;

use crate::{
    observe::SelectedPath,
    out::{Out, ms, path_name},
};

/// First bytes of a bulk transfer stream.
pub const SINK_MAGIC: &[u8] = b"golem-p2p-cli/sink/1\n";

/// Maximum size of an echo request.
pub const MAX_REQUEST: usize = 1 << 20;

/// Serves requests until the endpoint is closed.
pub async fn serve(ep: Endpoint, out: Out) {
    let endpoint_id = ep.endpoint_id().to_string();
    let instance = hex::encode(ep.instance_id());
    while let Some(inc) = ep.accept().await {
        let out = out.clone();
        let me = format!("endpoint={endpoint_id} instance={instance}");
        tokio::spawn(async move {
            let from = inc.from();
            if let Err(e) = handle(inc, &out, &me).await {
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

async fn handle(mut inc: IncomingStream, out: &Out, me: &str) -> anyhow::Result<()> {
    let (from, to) = (inc.from(), inc.to());
    let start = Instant::now();
    let (_, recv) = inc.streams();
    // Read just enough to tell a bulk transfer from an echo request.
    let mut head = vec![0u8; SINK_MAGIC.len()];
    let mut filled = 0;
    let mut finished = false;
    while filled < head.len() {
        match recv.read(&mut head[filled..]).await? {
            Some(n) => filled += n,
            None => {
                finished = true;
                break;
            }
        }
    }
    head.truncate(filled);

    if head == SINK_MAGIC {
        let mut n: u64 = 0;
        while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
            n += chunk.len() as u64;
        }
        let elapsed = start.elapsed();
        let path = path_name(SelectedPath::of(inc.connection()).kind);
        if out.json() {
            out.event("request")
                .field("from", from.to_string())
                .field("to", to.to_string())
                .field("path", path)
                .field("bytes", n)
                .field("kind", "sink")
                .field("ms", ms(elapsed))
                .emit();
        } else {
            eprintln!(
                "bulk from {from} to {to} via {path}: {n} bytes in {} ms",
                elapsed.as_millis()
            );
        }
        let reply = format!("{me} to={to} from={from} sink_bytes={n}");
        inc.respond(reply.as_bytes()).await?;
        return Ok(());
    }

    if !finished {
        let rest = recv.read_to_end(MAX_REQUEST - head.len()).await?;
        head.extend_from_slice(&rest);
    }
    let req = head;
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
        eprintln!(
            "request from {from} to {to} via {path}: {}",
            String::from_utf8_lossy(&req)
        );
    }
    let reply = format!(
        "{me} to={to} from={from} echo={}",
        String::from_utf8_lossy(&req)
    );
    inc.respond(reply.as_bytes()).await?;
    Ok(())
}
