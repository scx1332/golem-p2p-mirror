//! A minimal HTTP directory of self-signed node records (feature `directory-server`).
//!
//! The directory stores records keyed by every identity they list, using the
//! same [`RecordStore`] rules as a node: only verified records are accepted,
//! newer sequence numbers replace older ones, and storage is bounded. Clients
//! verify every record they fetch, so the directory needs no trust. It can
//! withhold records, but it cannot forge, merge or redirect identities.
//!
//! The server speaks plain HTTP. Run it behind a TLS-terminating proxy in
//! production. TLS protects metadata and availability here, not record
//! integrity, which comes from the signatures.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};

use crate::{
    identity::NodeId,
    record::{MAX_RECORD_SIZE, VerifiedRecord},
    store::{InsertOutcome, RecordStore},
};

/// Builds the directory's HTTP routes on top of `store`.
pub fn router(store: Arc<RecordStore>) -> Router {
    Router::new()
        .route("/v1/records", put(put_record))
        .route("/v1/records/{node_id}", get(get_record))
        .route("/health", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(MAX_RECORD_SIZE))
        .with_state(store)
}

/// Serves the directory on `listener` until the future is dropped.
///
/// Expired records are pruned once a minute.
pub async fn serve(
    listener: tokio::net::TcpListener,
    store: Arc<RecordStore>,
) -> std::io::Result<()> {
    let pruner = {
        let store = store.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                store.prune();
            }
        })
    };
    let res = axum::serve(listener, router(store)).await;
    pruner.abort();
    res
}

async fn put_record(State(store): State<Arc<RecordStore>>, body: Bytes) -> Response {
    let rec = match VerifiedRecord::verify(&body) {
        Ok(rec) => rec,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    match store.insert(&rec) {
        InsertOutcome::Updated | InsertOutcome::Unchanged => StatusCode::NO_CONTENT.into_response(),
        InsertOutcome::Stale => (StatusCode::CONFLICT, "a newer record is stored").into_response(),
        InsertOutcome::Expired => (StatusCode::BAD_REQUEST, "record expired").into_response(),
    }
}

async fn get_record(
    State(store): State<Arc<RecordStore>>,
    Path(node_id): Path<String>,
) -> Response {
    let Ok(id) = node_id.parse::<NodeId>() else {
        return (StatusCode::BAD_REQUEST, "invalid node id").into_response();
    };
    match store.resolve(&id) {
        Some(rec) => (
            [(header::CONTENT_TYPE, "application/octet-stream")],
            rec.to_bytes().to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
