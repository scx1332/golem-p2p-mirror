//! A bounded store of verified node records.
//!
//! Used as the peer cache inside an endpoint, by [`crate::discovery::MemoryDiscovery`]
//! and by the HTTP directory server. The rules are the same everywhere:
//!
//! * Only [`VerifiedRecord`]s can be stored, so the store never has to trust
//!   whoever handed it the record.
//! * Records are indexed by **every** identity they list and by their iroh
//!   endpoint ID. For each key, the record with the highest `seq` wins. A
//!   record that is older than what is stored for a key does not replace it
//!   (this is what prevents rollback to old endpoint keys or addresses).
//! * An identity is *withdrawn* when the endpoint that last listed it has since
//!   published a newer record without it (for example after switching the
//!   default identity or removing an alias). Withdrawn identities do not
//!   resolve, even though their older record is still signed and unexpired.
//! * The number of stored entries is bounded. When full, expired entries are
//!   dropped first, then the entries that expire soonest (1 % of the capacity
//!   at once, so that a full store does not scan all entries on every
//!   insert).
//!
//! Nothing here is keyed by an identity in a way that pins it to an endpoint:
//! a newer record for the same identity always replaces the older binding.

use std::{collections::HashMap, hash::Hash, sync::Mutex, time::SystemTime};

use iroh::EndpointId;

use crate::{identity::NodeId, record::VerifiedRecord};

/// Default maximum number of entries per index.
pub const DEFAULT_CAPACITY: usize = 4096;

/// Result of [`RecordStore::insert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// The record is now the latest for at least one of its keys.
    Updated,
    /// The same record was already stored.
    Unchanged,
    /// Every key already maps to a newer record; nothing was stored.
    Stale,
    /// The record has already expired.
    Expired,
}

/// A bounded, verified record store. See the module documentation.
#[derive(Debug)]
pub struct RecordStore {
    capacity: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    by_id: HashMap<NodeId, VerifiedRecord>,
    by_endpoint: HashMap<EndpointId, VerifiedRecord>,
}

impl Default for RecordStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

enum Slot {
    New,
    Newer,
    Same,
    Older,
}

fn compare(existing: Option<&VerifiedRecord>, rec: &VerifiedRecord) -> Slot {
    match existing {
        None => Slot::New,
        Some(e) if e.seq() < rec.seq() => Slot::Newer,
        Some(e) if e.seq() == rec.seq() && e == rec => Slot::Same,
        // Equal seq with different content: keep the first one seen. Honest
        // nodes never reuse a sequence number.
        Some(_) => Slot::Older,
    }
}

/// Makes room for one more entry: drops expired entries, then the entries
/// that expire soonest. When it has to evict, it evicts 1 % of the capacity
/// more than needed, so that one pass over the map (O(n)) makes room for many
/// inserts instead of one.
fn make_room<K: Eq + Hash + Copy>(
    map: &mut HashMap<K, VerifiedRecord>,
    capacity: usize,
    now: SystemTime,
) {
    if map.len() < capacity {
        return;
    }
    map.retain(|_, r| !r.is_expired_at(now));
    if map.len() < capacity {
        return;
    }
    let mut by_expiry: Vec<(SystemTime, K)> =
        map.iter().map(|(k, r)| (r.expires_at(), *k)).collect();
    let evict = (map.len() - capacity + 1 + capacity / 100).min(by_expiry.len());
    by_expiry.select_nth_unstable_by_key(evict - 1, |(t, _)| *t);
    for (_, k) in &by_expiry[..evict] {
        map.remove(k);
    }
}

impl RecordStore {
    /// Creates a store holding at most `capacity` identities and `capacity`
    /// endpoints.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::default(),
        }
    }

    /// Inserts a verified record, applying the sequence number rules.
    pub fn insert(&self, rec: &VerifiedRecord) -> InsertOutcome {
        self.insert_at(rec, SystemTime::now())
    }

    /// Like [`Self::insert`], at a given time.
    pub fn insert_at(&self, rec: &VerifiedRecord, now: SystemTime) -> InsertOutcome {
        if rec.is_expired_at(now) {
            return InsertOutcome::Expired;
        }
        let mut inner = self.inner.lock().expect("poisoned");
        let mut updated = false;
        let mut same = false;

        let ep = rec.endpoint_id();
        match compare(inner.by_endpoint.get(&ep), rec) {
            Slot::New => {
                make_room(&mut inner.by_endpoint, self.capacity, now);
                inner.by_endpoint.insert(ep, rec.clone());
                updated = true;
            }
            Slot::Newer => {
                inner.by_endpoint.insert(ep, rec.clone());
                updated = true;
            }
            Slot::Same => same = true,
            Slot::Older => {}
        }
        for id in rec.identities() {
            match compare(inner.by_id.get(id), rec) {
                Slot::New => {
                    make_room(&mut inner.by_id, self.capacity, now);
                    inner.by_id.insert(*id, rec.clone());
                    updated = true;
                }
                Slot::Newer => {
                    inner.by_id.insert(*id, rec.clone());
                    updated = true;
                }
                Slot::Same => same = true,
                Slot::Older => {}
            }
        }
        if updated {
            InsertOutcome::Updated
        } else if same {
            InsertOutcome::Unchanged
        } else {
            InsertOutcome::Stale
        }
    }

    /// Returns the current record for an identity, unless it expired or was
    /// withdrawn by a newer record of the same endpoint.
    pub fn resolve(&self, id: &NodeId) -> Option<VerifiedRecord> {
        self.resolve_at(id, SystemTime::now())
    }

    /// Like [`Self::resolve`], at a given time.
    pub fn resolve_at(&self, id: &NodeId, now: SystemTime) -> Option<VerifiedRecord> {
        let inner = self.inner.lock().expect("poisoned");
        let rec = inner.by_id.get(id)?;
        if rec.is_expired_at(now) {
            return None;
        }
        if let Some(latest) = inner.by_endpoint.get(&rec.endpoint_id())
            && latest.seq() > rec.seq()
            && !latest.contains(id)
        {
            return None;
        }
        Some(rec.clone())
    }

    /// Returns the latest record published by an endpoint.
    pub fn by_endpoint(&self, ep: &EndpointId) -> Option<VerifiedRecord> {
        let inner = self.inner.lock().expect("poisoned");
        inner
            .by_endpoint
            .get(ep)
            .filter(|r| !r.is_expired_at(SystemTime::now()))
            .cloned()
    }

    /// Removes expired entries.
    pub fn prune(&self) {
        let now = SystemTime::now();
        let mut inner = self.inner.lock().expect("poisoned");
        inner.by_id.retain(|_, r| !r.is_expired_at(now));
        inner.by_endpoint.retain(|_, r| !r.is_expired_at(now));
    }

    /// Returns the number of identities and endpoints stored.
    pub fn len(&self) -> (usize, usize) {
        let inner = self.inner.lock().expect("poisoned");
        (inner.by_id.len(), inner.by_endpoint.len())
    }

    /// Returns `true` if nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use iroh::SecretKey;

    use super::*;
    use crate::{
        record::{NodeRecord, RECORD_VERSION, sign_record},
        signer::InMemorySigner,
    };

    struct Node {
        key: SecretKey,
        signer: InMemorySigner,
        seq: u64,
    }

    impl Node {
        fn new(signer: &InMemorySigner) -> Self {
            Self {
                key: SecretKey::generate(),
                signer: signer.clone(),
                seq: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64,
            }
        }

        async fn record(&mut self, ids: &[NodeId], ttl: u64) -> VerifiedRecord {
            self.seq += 1;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let r = NodeRecord {
                version: RECORD_VERSION,
                seq: self.seq,
                expires_at: now + ttl,
                instance_id: [0; 16],
                endpoint_id: *self.key.public().as_bytes(),
                default_id: ids[0],
                identities: ids.to_vec(),
                relay_urls: vec![],
                direct_addrs: vec![],
            };
            sign_record(&r, &self.key, &self.signer).await.unwrap()
        }
    }

    #[tokio::test]
    async fn newer_replaces_older_never_reverse() {
        let s = InMemorySigner::new();
        let a = s.generate();
        let mut n = Node::new(&s);
        let r1 = n.record(&[a], 3600).await;
        let r2 = n.record(&[a], 3600).await;
        let store = RecordStore::default();
        assert_eq!(store.insert(&r1), InsertOutcome::Updated);
        assert_eq!(store.insert(&r2), InsertOutcome::Updated);
        assert_eq!(store.insert(&r1), InsertOutcome::Stale);
        assert_eq!(store.insert(&r2), InsertOutcome::Unchanged);
        assert_eq!(store.resolve(&a), Some(r2));
    }

    #[tokio::test]
    async fn identity_moves_to_new_endpoint() {
        // Restart with a fresh endpoint key: the newer record wins and the old
        // endpoint is not reachable through the identity any more.
        let s = InMemorySigner::new();
        let a = s.generate();
        let mut old = Node::new(&s);
        let r_old = old.record(&[a], 3600).await;
        let mut new = Node::new(&s);
        new.seq = old.seq + 10;
        let r_new = new.record(&[a], 3600).await;
        let store = RecordStore::default();
        store.insert(&r_old);
        store.insert(&r_new);
        assert_eq!(store.resolve(&a).unwrap().endpoint_id(), new.key.public());
        // Replaying the old record does not roll the binding back. (It is
        // still the newest record of the old endpoint key, hence `Unchanged`.)
        assert_ne!(store.insert(&r_old), InsertOutcome::Updated);
        assert_eq!(store.resolve(&a).unwrap().endpoint_id(), new.key.public());
    }

    #[tokio::test]
    async fn switch_default_and_back_withdraws_correctly() {
        let s = InMemorySigner::new();
        let (a, b) = (s.generate(), s.generate());
        let mut n = Node::new(&s);
        let store = RecordStore::default();
        let r1 = n.record(&[a], 3600).await;
        store.insert(&r1);
        assert!(store.resolve(&a).is_some());
        assert!(store.resolve(&b).is_none());

        // A -> B: a is withdrawn by the same endpoint's newer record.
        let r2 = n.record(&[b], 3600).await;
        store.insert(&r2);
        assert!(store.resolve(&a).is_none());
        assert_eq!(store.resolve(&b), Some(r2.clone()));

        // B -> A: a is back, b withdrawn.
        let r3 = n.record(&[a], 3600).await;
        store.insert(&r3);
        assert_eq!(store.resolve(&a), Some(r3.clone()));
        assert!(store.resolve(&b).is_none());

        // Out-of-order delivery of r2 changes nothing.
        assert_ne!(store.insert(&r2), InsertOutcome::Updated);
        assert!(store.resolve(&b).is_none());
        assert_eq!(store.resolve(&a), Some(r3));

        // Default switch keeping the old identity as an alias.
        let r4 = n.record(&[b, a], 3600).await;
        store.insert(&r4);
        assert_eq!(store.resolve(&a), Some(r4.clone()));
        assert_eq!(store.resolve(&b).unwrap().default_id(), b);
    }

    #[tokio::test]
    async fn bounded() {
        let s = InMemorySigner::new();
        let store = RecordStore::new(8);
        for i in 0..50u64 {
            let id = s.generate();
            let mut n = Node::new(&s);
            let r = n.record(&[id], 600 + i).await;
            store.insert(&r);
            let (ids, eps) = store.len();
            assert!(ids <= 8 && eps <= 8);
        }
        // Soonest-expiring entries were evicted; the latest ones remain.
        assert_eq!(store.len(), (8, 8));
    }

    #[tokio::test]
    async fn eviction_in_batches_keeps_latest_expiring() {
        let s = InMemorySigner::new();
        let store = RecordStore::new(200);
        let mut recs = Vec::new();
        for i in 0..230u64 {
            let id = s.generate();
            let mut n = Node::new(&s);
            recs.push((id, n.record(&[id], 600 + i).await));
        }
        for (_, r) in &recs {
            store.insert(r);
            let (ids, eps) = store.len();
            assert!(ids <= 200 && eps <= 200);
        }
        // Each eviction makes room for 1 % of the capacity at once.
        let (ids, _) = store.len();
        assert!(ids >= 196, "{ids}");
        // Only the soonest-expiring records were evicted.
        let kept: Vec<bool> = recs
            .iter()
            .map(|(id, _)| store.resolve(id).is_some())
            .collect();
        let first_kept = kept.iter().position(|k| *k).unwrap();
        assert!(kept[first_kept..].iter().all(|k| *k), "{kept:?}");
    }

    #[tokio::test]
    async fn expired_not_resolved() {
        let s = InMemorySigner::new();
        let a = s.generate();
        let mut n = Node::new(&s);
        let r = n.record(&[a], 60).await;
        let store = RecordStore::default();
        store.insert(&r);
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert!(store.resolve_at(&a, later).is_none());
        assert_eq!(store.insert_at(&r, later), InsertOutcome::Expired);
    }
}
