//! Integration with iroh's address lookup.
//!
//! iroh resolves endpoint IDs to transport addresses through its
//! [`AddressLookup`] trait. [`RecordLookup`] answers those queries from the
//! endpoint's store of *verified* node records, so whenever iroh needs fresh
//! addresses for a peer (for example while dialing or hole punching), it gets
//! exactly the relay URLs and direct addresses that the peer signed, and
//! nothing a relay or directory made up.

use std::sync::Arc;

use iroh::{
    EndpointId,
    address_lookup::{AddressLookup, EndpointInfo, Error, Item},
};
use n0_future::{boxed::BoxStream, stream};

use crate::store::RecordStore;

/// Provenance string reported to iroh.
pub const PROVENANCE: &str = "golem-p2p-records";

/// An iroh [`AddressLookup`] backed by a [`RecordStore`] of verified records.
#[derive(Debug, Clone)]
pub struct RecordLookup {
    store: Arc<RecordStore>,
}

impl RecordLookup {
    /// Creates a lookup over `store`.
    pub fn new(store: Arc<RecordStore>) -> Self {
        Self { store }
    }
}

impl AddressLookup for RecordLookup {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        let rec = self.store.by_endpoint(&endpoint_id)?;
        let info = EndpointInfo::from(rec.endpoint_addr());
        let item = Item::new(info, PROVENANCE, Some(rec.seq()));
        Some(Box::pin(stream::once(Ok(item))))
    }
}
