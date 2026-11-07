//! Discovery: from an Ethereum node ID to a verified record, then to an iroh address.
//!
//! A [`Discovery`] publishes our own signed record and looks up the record of
//! a node ID. Because records are self-signed by every identity they list and
//! by the endpoint key, a discovery service needs no trust: the worst it can do
//! is withhold records or serve old ones, and old ones lose against newer ones
//! in the [`RecordStore`]. Lookups return [`VerifiedRecord`], a type that can
//! only be built by verifying signatures, so every implementation is forced to
//! verify what it received.
//!
//! Implementations here:
//! * [`MemoryDiscovery`]: an in-process shared store, for tests.
//! * [`HttpDirectory`]: a client for the small HTTP directory in
//!   [`crate::directory`] (feature `http-directory`), which works across machines.
//! * [`MultiDiscovery`]: publish to several services, look up in all of them
//!   and keep the newest valid record.

use std::{fmt, sync::Arc};

use crate::{
    identity::NodeId,
    record::VerifiedRecord,
    signer::BoxFuture,
    store::{InsertOutcome, RecordStore},
};

#[cfg(feature = "http-directory")]
pub use self::http::HttpDirectory;

/// Error from a discovery service.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DiscoveryError {
    /// The service could not be reached or answered with an error.
    #[error("discovery service error: {0}")]
    Service(String),
    /// The service returned a record that failed verification.
    #[error("discovery returned an invalid record: {0}")]
    InvalidRecord(#[from] crate::record::RecordError),
    /// The service rejected our record because it already has a newer one.
    #[error("discovery service holds a newer record")]
    Stale,
}

/// Maps node IDs to verified records. See the module documentation.
pub trait Discovery: Send + Sync + fmt::Debug + 'static {
    /// Publishes our current record.
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>>;

    /// Looks up the latest record listing `id`. Returns `Ok(None)` if unknown.
    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>>;
}

impl<T: Discovery + ?Sized> Discovery for Arc<T> {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        (**self).publish(record)
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        (**self).lookup(id)
    }
}

/// An in-process discovery service backed by a shared [`RecordStore`].
///
/// Clones share the same store, so several endpoints in one test can find
/// each other.
#[derive(Debug, Clone, Default)]
pub struct MemoryDiscovery {
    store: Arc<RecordStore>,
}

impl MemoryDiscovery {
    /// Creates an empty in-memory discovery service.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the backing store.
    pub fn store(&self) -> &Arc<RecordStore> {
        &self.store
    }
}

impl Discovery for MemoryDiscovery {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        let res = match self.store.insert(record) {
            InsertOutcome::Updated | InsertOutcome::Unchanged => Ok(()),
            InsertOutcome::Stale => Err(DiscoveryError::Stale),
            InsertOutcome::Expired => Err(DiscoveryError::InvalidRecord(
                crate::record::RecordError::Expiry,
            )),
        };
        Box::pin(async move { res })
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        let res = self.store.resolve(&id);
        Box::pin(async move { Ok(res) })
    }
}

/// Combines several discovery services.
///
/// Publishing succeeds if at least one service accepted the record. Lookups
/// query all services and return the valid record with the highest sequence
/// number, so a single stale or withholding service cannot hide a newer record
/// held by another.
#[derive(Debug, Clone, Default)]
pub struct MultiDiscovery {
    services: Vec<Arc<dyn Discovery>>,
}

impl MultiDiscovery {
    /// Creates an empty combinator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a service.
    pub fn with(mut self, service: impl Discovery) -> Self {
        self.services.push(Arc::new(service));
        self
    }

    /// Adds an already shared service.
    pub fn with_arc(mut self, service: Arc<dyn Discovery>) -> Self {
        self.services.push(service);
        self
    }
}

impl Discovery for MultiDiscovery {
    fn publish<'a>(
        &'a self,
        record: &'a VerifiedRecord,
    ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
        Box::pin(async move {
            let futs = self.services.iter().map(|s| s.publish(record));
            let results = n0_future::join_all(futs).await;
            let total = results.len();
            let ok = results.iter().any(|r| r.is_ok());
            let mut last_err = None;
            for (i, r) in results.into_iter().enumerate() {
                if let Err(e) = r {
                    // The caller learns about at most one failure, and about
                    // none if another service accepted the record.
                    let n = i + 1;
                    if ok {
                        tracing::warn!(
                            seq = record.seq(),
                            "publishing to discovery service {n} of {total} failed: {e}"
                        );
                    } else {
                        tracing::debug!(
                            seq = record.seq(),
                            "publishing to discovery service {n} of {total} failed: {e}"
                        );
                    }
                    last_err = Some(e);
                }
            }
            match (ok, last_err) {
                (true, _) | (false, None) => Ok(()),
                (false, Some(e)) => Err(e),
            }
        })
    }

    fn lookup(&self, id: NodeId) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
        Box::pin(async move {
            let futs = self.services.iter().map(|s| s.lookup(id));
            let results = n0_future::join_all(futs).await;
            let total = results.len();
            let mut best: Option<VerifiedRecord> = None;
            let mut last_err = None;
            let mut any_ok = false;
            for (i, r) in results.into_iter().enumerate() {
                if let Err(e) = &r {
                    tracing::debug!(%id, "lookup in discovery service {} of {total} failed: {e}", i + 1);
                }
                match r {
                    Ok(Some(rec)) if rec.contains(&id) => {
                        any_ok = true;
                        if best.as_ref().is_none_or(|b| b.seq() < rec.seq()) {
                            best = Some(rec);
                        }
                    }
                    Ok(_) => any_ok = true,
                    Err(e) => last_err = Some(e),
                }
            }
            match (best, any_ok, last_err) {
                (Some(b), _, _) => Ok(Some(b)),
                (None, true, _) | (None, false, None) => Ok(None),
                (None, false, Some(e)) => Err(e),
            }
        })
    }
}

#[cfg(feature = "http-directory")]
mod http {
    use std::time::Duration;

    use super::*;
    use crate::record::MAX_RECORD_SIZE;

    /// Client for the HTTP directory served by [`crate::directory`].
    ///
    /// Protocol:
    /// * `PUT  {base}/v1/records` with the encoded signed record as body.
    ///   `204` stored, `409` the directory has a newer record, `400` invalid.
    /// * `GET  {base}/v1/records/0x<node id>` returns the encoded record or `404`.
    ///
    /// Every record received is verified here; the directory is not trusted.
    #[derive(Debug, Clone)]
    pub struct HttpDirectory {
        base: url::Url,
        client: reqwest::Client,
    }

    impl HttpDirectory {
        /// Creates a client for the directory at `base` (for example
        /// `https://directory.example.org/`), trusting the standard web PKI
        /// roots for HTTPS.
        pub fn new(base: url::Url) -> Result<Self, DiscoveryError> {
            Self::with_tls(base, iroh::tls::CaTlsConfig::default())
        }

        /// Like [`Self::new`], with explicit TLS roots.
        pub fn with_tls(
            base: url::Url,
            tls: iroh::tls::CaTlsConfig,
        ) -> Result<Self, DiscoveryError> {
            let tls = tls
                .client_config(iroh::tls::default_provider())
                .map_err(|e| DiscoveryError::Service(e.to_string()))?;
            let client = reqwest::Client::builder()
                .tls_backend_preconfigured(tls)
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| DiscoveryError::Service(e.to_string()))?;
            Ok(Self { base, client })
        }

        fn url(&self, path: &str) -> Result<url::Url, DiscoveryError> {
            self.base
                .join(path)
                .map_err(|e| DiscoveryError::Service(e.to_string()))
        }

        /// A service error that names this directory.
        fn error(&self, e: impl fmt::Display) -> DiscoveryError {
            DiscoveryError::Service(format!("directory {}: {e}", self.base))
        }

        async fn read_limited(mut resp: reqwest::Response) -> Result<Vec<u8>, String> {
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                if body.len() + chunk.len() > MAX_RECORD_SIZE {
                    return Err("response too large".into());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        }
    }

    impl Discovery for HttpDirectory {
        fn publish<'a>(
            &'a self,
            record: &'a VerifiedRecord,
        ) -> BoxFuture<'a, Result<(), DiscoveryError>> {
            Box::pin(async move {
                let resp = self
                    .client
                    .put(self.url("v1/records")?)
                    .header("content-type", "application/octet-stream")
                    .body(record.to_bytes().to_vec())
                    .send()
                    .await
                    // reqwest's error names the URL already.
                    .map_err(|e| DiscoveryError::Service(e.to_string()))?;
                match resp.status().as_u16() {
                    200..=299 => Ok(()),
                    409 => Err(DiscoveryError::Stale),
                    s => Err(self.error(format!("publish failed with HTTP {s}"))),
                }
            })
        }

        fn lookup(
            &self,
            id: NodeId,
        ) -> BoxFuture<'_, Result<Option<VerifiedRecord>, DiscoveryError>> {
            Box::pin(async move {
                let resp = self
                    .client
                    .get(self.url(&format!("v1/records/{id}"))?)
                    .send()
                    .await
                    // reqwest's error names the URL already.
                    .map_err(|e| DiscoveryError::Service(e.to_string()))?;
                match resp.status().as_u16() {
                    404 => Ok(None),
                    200 => {
                        let body = Self::read_limited(resp).await.map_err(|e| self.error(e))?;
                        let rec = VerifiedRecord::verify(&body)?;
                        if !rec.contains(&id) {
                            return Err(self.error("returned a record for another identity"));
                        }
                        Ok(Some(rec))
                    }
                    s => Err(self.error(format!("lookup failed with HTTP {s}"))),
                }
            })
        }
    }
}
