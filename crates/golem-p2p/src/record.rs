//! Self-signed node records that bind Ethereum identities to an iroh endpoint.
//!
//! A [`NodeRecord`] states: "the node whose identities are `identities` (default
//! `default_id`) is reachable at iroh endpoint `endpoint_id`, via `relay_urls`
//! and `direct_addrs`, until `expires_at`". The record is encoded once, hashed
//! with a domain tag, and the digest is signed:
//!
//! * by **every** listed identity (secp256k1, yagna format), so no relay,
//!   directory or other party can add an identity to a node, group identities,
//!   or point an identity at a different endpoint key;
//! * by the iroh endpoint key (ed25519), so nobody can claim someone else's
//!   endpoint for their own identities.
//!
//! A peer accepts a record only if all signatures verify. Among valid records,
//! a higher `seq` replaces a lower one; see [`crate::store`].
//!
//! Relay URLs in a record must use `https` ([`RELAY_URL_SCHEME`]). A record
//! listing any other scheme, including plain `http`, does not verify, so a node
//! can neither publish nor learn a plain-http relay.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey, TransportAddr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    identity::{NodeId, Signature, SignatureError},
    signer::{SignError, Signer},
};

/// Current record format version.
pub const RECORD_VERSION: u8 = 1;

/// Domain separation tag hashed in front of the encoded record.
pub const RECORD_DOMAIN: &[u8] = b"golem-p2p/node-record/v1\0";

/// Maximum number of identities in one record.
pub const MAX_IDENTITIES: usize = 16;
/// Maximum number of relay URLs in one record.
pub const MAX_RELAYS: usize = 8;
/// Maximum number of direct addresses in one record.
pub const MAX_DIRECT_ADDRS: usize = 16;
/// Maximum length of one relay URL.
pub const MAX_URL_LEN: usize = 256;
/// The only scheme allowed for relay URLs. Relay connections always use TLS.
pub const RELAY_URL_SCHEME: &str = "https";
/// Maximum size of an encoded signed record.
pub const MAX_RECORD_SIZE: usize = 4096;
/// Maximum lifetime a record may claim.
pub const MAX_TTL: Duration = Duration::from_secs(24 * 3600);
/// Tolerated clock difference between nodes.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(300);

/// The signed content of a node record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRecord {
    /// Format version, currently [`RECORD_VERSION`].
    pub version: u8,
    /// Sequence number. Higher replaces lower. Microseconds since the Unix
    /// epoch at signing time, bumped if needed to stay strictly increasing.
    pub seq: u64,
    /// Expiry, in seconds since the Unix epoch.
    pub expires_at: u64,
    /// Random ID of the publishing process, new on every start.
    pub instance_id: [u8; 16],
    /// The iroh endpoint ID (ed25519 public key) the identities are bound to.
    pub endpoint_id: [u8; 32],
    /// The default identity. Must also appear in `identities`.
    pub default_id: NodeId,
    /// All identities of the node, default first.
    pub identities: Vec<NodeId>,
    /// Relay URLs where the endpoint can be reached. Must be `https` URLs.
    pub relay_urls: Vec<String>,
    /// Direct UDP addresses of the endpoint.
    pub direct_addrs: Vec<SocketAddr>,
}

#[derive(Serialize, Deserialize)]
struct SignedWire {
    payload: Vec<u8>,
    endpoint_sig: Vec<u8>,
    identity_sigs: Vec<Vec<u8>>,
}

/// Why a record was rejected.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RecordError {
    /// The encoding is invalid or exceeds a size limit.
    #[error("malformed record: {0}")]
    Malformed(&'static str),
    /// Unsupported format version.
    #[error("unsupported record version {0}")]
    Version(u8),
    /// The record has expired, or claims a lifetime longer than allowed.
    #[error("record expired or expiry out of range")]
    Expiry,
    /// The sequence number lies too far in the future.
    #[error("record sequence number is in the future")]
    FutureSeq,
    /// The endpoint signature is missing or invalid.
    #[error("invalid endpoint signature")]
    EndpointSignature,
    /// An identity's signature is missing, invalid, or made by another key.
    #[error("identity {0} did not sign this record")]
    IdentitySignature(NodeId),
    /// A signature could not be decoded.
    #[error("bad signature: {0}")]
    Signature(#[from] SignatureError),
    /// The signer refused or failed to sign.
    #[error("signing failed: {0}")]
    Sign(#[from] SignError),
}

fn now_since_epoch(now: SystemTime) -> Duration {
    now.duration_since(UNIX_EPOCH).unwrap_or_default()
}

impl NodeRecord {
    /// Returns the digest that every party signs.
    pub fn digest_of(payload: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(RECORD_DOMAIN);
        h.update(payload);
        h.finalize().into()
    }

    /// Checks the record's own invariants (no signatures involved).
    fn validate(&self, now: SystemTime) -> Result<(), RecordError> {
        if self.version != RECORD_VERSION {
            return Err(RecordError::Version(self.version));
        }
        if self.identities.is_empty() || self.identities.len() > MAX_IDENTITIES {
            return Err(RecordError::Malformed("identity count"));
        }
        if self.identities[0] != self.default_id {
            return Err(RecordError::Malformed(
                "default identity must be listed first",
            ));
        }
        let mut sorted = self.identities.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.identities.len() {
            return Err(RecordError::Malformed("duplicate identity"));
        }
        if self.relay_urls.len() > MAX_RELAYS {
            return Err(RecordError::Malformed("too many relay urls"));
        }
        for url in &self.relay_urls {
            if url.len() > MAX_URL_LEN {
                return Err(RecordError::Malformed("relay url too long"));
            }
            let parsed: url::Url = url
                .parse()
                .map_err(|_| RecordError::Malformed("relay url"))?;
            if parsed.scheme() != RELAY_URL_SCHEME {
                return Err(RecordError::Malformed("relay url must use https"));
            }
        }
        if self.direct_addrs.len() > MAX_DIRECT_ADDRS {
            return Err(RecordError::Malformed("too many direct addresses"));
        }
        let now = now_since_epoch(now);
        let expires = Duration::from_secs(self.expires_at);
        if expires + MAX_CLOCK_SKEW < now || expires > now + MAX_TTL + MAX_CLOCK_SKEW {
            return Err(RecordError::Expiry);
        }
        if u128::from(self.seq) > (now + MAX_CLOCK_SKEW).as_micros() {
            return Err(RecordError::FutureSeq);
        }
        Ok(())
    }
}

/// A node record whose signatures have all been verified.
///
/// The only ways to obtain one are [`VerifiedRecord::verify`] (for records
/// from the network) and [`sign_record`] (for our own records, which are
/// verified after signing as well).
#[derive(Debug, Clone)]
pub struct VerifiedRecord {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    record: NodeRecord,
    endpoint_id: EndpointId,
    bytes: Vec<u8>,
}

impl PartialEq for VerifiedRecord {
    fn eq(&self, other: &Self) -> bool {
        self.inner.bytes == other.inner.bytes
    }
}
impl Eq for VerifiedRecord {}

impl VerifiedRecord {
    /// Decodes and verifies an encoded signed record at the current time.
    pub fn verify(bytes: &[u8]) -> Result<Self, RecordError> {
        Self::verify_at(bytes, SystemTime::now())
    }

    /// Decodes and verifies an encoded signed record at the given time.
    pub fn verify_at(bytes: &[u8], now: SystemTime) -> Result<Self, RecordError> {
        if bytes.len() > MAX_RECORD_SIZE {
            return Err(RecordError::Malformed("record too large"));
        }
        let (wire, rest): (SignedWire, _) =
            postcard::take_from_bytes(bytes).map_err(|_| RecordError::Malformed("encoding"))?;
        if !rest.is_empty() {
            return Err(RecordError::Malformed("trailing bytes"));
        }
        let (record, rest): (NodeRecord, _) = postcard::take_from_bytes(&wire.payload)
            .map_err(|_| RecordError::Malformed("payload encoding"))?;
        if !rest.is_empty() {
            return Err(RecordError::Malformed("trailing payload bytes"));
        }
        record.validate(now)?;

        let digest = NodeRecord::digest_of(&wire.payload);

        let endpoint_id = EndpointId::from_bytes(&record.endpoint_id)
            .map_err(|_| RecordError::Malformed("endpoint id"))?;
        let sig: [u8; 64] = wire
            .endpoint_sig
            .as_slice()
            .try_into()
            .map_err(|_| RecordError::EndpointSignature)?;
        endpoint_id
            .verify(&digest, &iroh::Signature::from_bytes(&sig))
            .map_err(|_| RecordError::EndpointSignature)?;

        if wire.identity_sigs.len() != record.identities.len() {
            // Every listed identity must sign; extra signatures are not allowed either.
            let missing = record
                .identities
                .get(wire.identity_sigs.len())
                .copied()
                .unwrap_or(record.default_id);
            return Err(RecordError::IdentitySignature(missing));
        }
        for (id, sig) in record.identities.iter().zip(&wire.identity_sigs) {
            let sig = Signature::from_slice(sig)?;
            match sig.recover_node_id(&digest) {
                Ok(signer) if signer == *id => {}
                _ => return Err(RecordError::IdentitySignature(*id)),
            }
        }

        Ok(Self {
            inner: Arc::new(Inner {
                record,
                endpoint_id,
                bytes: bytes.to_vec(),
            }),
        })
    }

    /// Returns the verified record content.
    pub fn record(&self) -> &NodeRecord {
        &self.inner.record
    }

    /// Returns the encoded signed record, exactly as verified.
    pub fn to_bytes(&self) -> &[u8] {
        &self.inner.bytes
    }

    /// Returns the iroh endpoint ID the identities are bound to.
    pub fn endpoint_id(&self) -> EndpointId {
        self.inner.endpoint_id
    }

    /// Returns the default identity.
    pub fn default_id(&self) -> NodeId {
        self.inner.record.default_id
    }

    /// Returns all identities, default first.
    pub fn identities(&self) -> &[NodeId] {
        &self.inner.record.identities
    }

    /// Returns `true` if `id` is one of the record's identities.
    pub fn contains(&self, id: &NodeId) -> bool {
        self.inner.record.identities.contains(id)
    }

    /// Returns the sequence number.
    pub fn seq(&self) -> u64 {
        self.inner.record.seq
    }

    /// Returns the publishing process' instance ID.
    pub fn instance_id(&self) -> [u8; 16] {
        self.inner.record.instance_id
    }

    /// Returns the expiry time.
    pub fn expires_at(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.inner.record.expires_at)
    }

    /// Returns `true` if the record has expired at `now`.
    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        self.expires_at() + MAX_CLOCK_SKEW < now
    }

    /// Returns the relay URLs that parse as iroh relay URLs.
    pub fn relay_urls(&self) -> Vec<RelayUrl> {
        self.inner
            .record
            .relay_urls
            .iter()
            .filter_map(|u| u.parse().ok())
            .collect()
    }

    /// Builds the iroh address to dial this record's endpoint.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        let relays = self.relay_urls().into_iter().map(TransportAddr::Relay);
        let ips = self
            .inner
            .record
            .direct_addrs
            .iter()
            .copied()
            .map(TransportAddr::Ip);
        EndpointAddr::from_parts(self.endpoint_id(), relays.chain(ips))
    }
}

/// Signs `record` with the endpoint key and every listed identity.
///
/// Fails if any identity cannot sign (unknown or locked key). The result is
/// verified before it is returned.
pub async fn sign_record(
    record: &NodeRecord,
    endpoint_key: &SecretKey,
    signer: &dyn Signer,
) -> Result<VerifiedRecord, RecordError> {
    if record.endpoint_id != *endpoint_key.public().as_bytes() {
        return Err(RecordError::Malformed(
            "endpoint id does not match endpoint key",
        ));
    }
    let payload = postcard::to_allocvec(record).map_err(|_| RecordError::Malformed("encoding"))?;
    let digest = NodeRecord::digest_of(&payload);
    let endpoint_sig = endpoint_key.sign(&digest).to_bytes().to_vec();
    let mut identity_sigs = Vec::with_capacity(record.identities.len());
    for id in &record.identities {
        let sig = signer.sign(*id, digest).await?;
        identity_sigs.push(sig.as_bytes().to_vec());
    }
    let wire = SignedWire {
        payload,
        endpoint_sig,
        identity_sigs,
    };
    let bytes = postcard::to_allocvec(&wire).map_err(|_| RecordError::Malformed("encoding"))?;
    VerifiedRecord::verify(&bytes)
}

/// Low-level helpers to build records with arbitrary (also invalid)
/// signatures. Only meant for tests of the verification logic.
#[doc(hidden)]
pub mod testing {
    use super::*;

    /// Encodes a record with explicitly given signatures, without checks.
    pub fn encode_raw(
        record: &NodeRecord,
        endpoint_sig: [u8; 64],
        identity_sigs: Vec<Signature>,
    ) -> Vec<u8> {
        let wire = SignedWire {
            payload: postcard::to_allocvec(record).expect("encode"),
            endpoint_sig: endpoint_sig.to_vec(),
            identity_sigs: identity_sigs
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
        };
        postcard::to_allocvec(&wire).expect("encode")
    }

    /// Returns the digest that signers sign for `record`.
    pub fn digest(record: &NodeRecord) -> [u8; 32] {
        NodeRecord::digest_of(&postcard::to_allocvec(record).expect("encode"))
    }

    /// Splits an encoded record into payload, endpoint signature and identity signatures.
    pub fn decode_raw(bytes: &[u8]) -> (NodeRecord, Vec<u8>, Vec<Vec<u8>>) {
        let wire: SignedWire = postcard::from_bytes(bytes).expect("decode");
        let record = postcard::from_bytes(&wire.payload).expect("decode");
        (record, wire.endpoint_sig, wire.identity_sigs)
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::*, *};
    use crate::signer::InMemorySigner;

    fn now_secs() -> u64 {
        now_since_epoch(SystemTime::now()).as_secs()
    }

    fn base(key: &SecretKey, ids: &[NodeId]) -> NodeRecord {
        NodeRecord {
            version: RECORD_VERSION,
            seq: now_since_epoch(SystemTime::now()).as_micros() as u64,
            expires_at: now_secs() + 3600,
            instance_id: [1; 16],
            endpoint_id: *key.public().as_bytes(),
            default_id: ids[0],
            identities: ids.to_vec(),
            relay_urls: vec!["https://relay.example.org./".into()],
            direct_addrs: vec!["127.0.0.1:1234".parse().unwrap()],
        }
    }

    async fn setup() -> (InMemorySigner, SecretKey, NodeId, NodeId) {
        let signer = InMemorySigner::new();
        let a = signer.generate();
        let b = signer.generate();
        (signer, SecretKey::generate(), a, b)
    }

    #[tokio::test]
    async fn sign_and_verify() {
        let (signer, key, a, b) = setup().await;
        let rec = sign_record(&base(&key, &[a, b]), &key, &signer)
            .await
            .unwrap();
        let again = VerifiedRecord::verify(rec.to_bytes()).unwrap();
        assert_eq!(again.identities(), &[a, b]);
        assert_eq!(again.default_id(), a);
        assert_eq!(again.endpoint_id(), key.public());
        assert_eq!(again.endpoint_addr().relay_urls().count(), 1);
    }

    #[tokio::test]
    async fn tampered_payload_rejected() {
        let (signer, key, a, _) = setup().await;
        let rec = sign_record(&base(&key, &[a]), &key, &signer).await.unwrap();
        let (mut r, esig, isigs) = decode_raw(rec.to_bytes());
        r.direct_addrs = vec!["10.0.0.1:1".parse().unwrap()];
        let isigs = isigs
            .iter()
            .map(|s| Signature::from_slice(s).unwrap())
            .collect();
        let forged = encode_raw(&r, esig.try_into().unwrap(), isigs);
        assert_eq!(
            VerifiedRecord::verify(&forged),
            Err(RecordError::EndpointSignature)
        );
    }

    #[tokio::test]
    async fn unsigned_identity_rejected() {
        let (signer, key, a, b) = setup().await;
        // Record lists a and b, but only a's signature is attached.
        let r = base(&key, &[a, b]);
        let d = digest(&r);
        let esig = key.sign(&d).to_bytes();
        let sa = signer.sign_blocking(a, &d).unwrap();
        let bytes = encode_raw(&r, esig, vec![sa]);
        assert_eq!(
            VerifiedRecord::verify(&bytes),
            Err(RecordError::IdentitySignature(b))
        );
        // b's slot signed by a instead of b.
        let bytes = encode_raw(&r, esig, vec![sa, sa]);
        assert_eq!(
            VerifiedRecord::verify(&bytes),
            Err(RecordError::IdentitySignature(b))
        );
        // An identity the attacker controls cannot vouch for one it does not.
        let attacker = signer.generate();
        let sx = signer.sign_blocking(attacker, &d).unwrap();
        let bytes = encode_raw(&r, esig, vec![sa, sx]);
        assert_eq!(
            VerifiedRecord::verify(&bytes),
            Err(RecordError::IdentitySignature(b))
        );
    }

    #[tokio::test]
    async fn endpoint_must_sign() {
        let (signer, key, a, _) = setup().await;
        // Someone with identity `a` claims an endpoint whose key they do not hold.
        let victim = SecretKey::generate();
        let mut r = base(&key, &[a]);
        r.endpoint_id = *victim.public().as_bytes();
        let d = digest(&r);
        let sa = signer.sign_blocking(a, &d).unwrap();
        let bytes = encode_raw(&r, key.sign(&d).to_bytes(), vec![sa]);
        assert_eq!(
            VerifiedRecord::verify(&bytes),
            Err(RecordError::EndpointSignature)
        );
        // sign_record refuses a mismatching key up front.
        assert!(sign_record(&r, &key, &signer).await.is_err());
    }

    #[tokio::test]
    async fn expiry_and_seq_bounds() {
        let (signer, key, a, _) = setup().await;
        let mut r = base(&key, &[a]);
        r.expires_at = now_secs() - 3600;
        assert_eq!(
            sign_record(&r, &key, &signer).await,
            Err(RecordError::Expiry)
        );
        let mut r = base(&key, &[a]);
        r.expires_at = now_secs() + 30 * 24 * 3600;
        assert_eq!(
            sign_record(&r, &key, &signer).await,
            Err(RecordError::Expiry)
        );
        let mut r = base(&key, &[a]);
        r.seq = u64::MAX;
        assert_eq!(
            sign_record(&r, &key, &signer).await,
            Err(RecordError::FutureSeq)
        );

        // A valid record is rejected once it has expired.
        let rec = sign_record(&base(&key, &[a]), &key, &signer).await.unwrap();
        let later = SystemTime::now() + Duration::from_secs(2 * 3600);
        assert_eq!(
            VerifiedRecord::verify_at(rec.to_bytes(), later),
            Err(RecordError::Expiry)
        );
    }

    #[tokio::test]
    async fn invariants() {
        let (signer, key, a, b) = setup().await;
        let mut r = base(&key, &[a, b]);
        r.default_id = b;
        assert!(matches!(
            sign_record(&r, &key, &signer).await,
            Err(RecordError::Malformed(_))
        ));
        let r = base(&key, &[a, a]);
        assert!(matches!(
            sign_record(&r, &key, &signer).await,
            Err(RecordError::Malformed(_))
        ));
        // Relay URLs must be https: plain http, ws and other schemes fail.
        for url in [
            "file:///etc/passwd",
            "http://relay.example.org./",
            "HTTP://relay.example.org./",
            "ws://relay.example.org./",
        ] {
            let mut r = base(&key, &[a]);
            r.relay_urls = vec!["https://ok.example.org./".into(), url.into()];
            assert_eq!(
                sign_record(&r, &key, &signer).await,
                Err(RecordError::Malformed("relay url must use https")),
                "{url}"
            );
        }
        // Locked key: signing fails, nothing is produced.
        signer.lock(b);
        assert_eq!(
            sign_record(&base(&key, &[a, b]), &key, &signer).await,
            Err(RecordError::Sign(SignError::Locked(b)))
        );
        // Oversized / trailing garbage.
        let rec = sign_record(&base(&key, &[a]), &key, &signer).await.unwrap();
        let mut bytes = rec.to_bytes().to_vec();
        bytes.push(0);
        assert!(VerifiedRecord::verify(&bytes).is_err());
        assert!(VerifiedRecord::verify(&vec![0u8; MAX_RECORD_SIZE + 1]).is_err());
    }

    /// A record listing a plain-http relay fails verification even when every
    /// signature on it is valid, so peers never dial such a relay.
    #[tokio::test]
    async fn http_relay_fails_verification() {
        let (signer, key, a, _) = setup().await;
        let mut r = base(&key, &[a]);
        r.relay_urls = vec!["http://relay.example.org./".into()];
        let d = digest(&r);
        let sa = signer.sign_blocking(a, &d).unwrap();
        let bytes = encode_raw(&r, key.sign(&d).to_bytes(), vec![sa]);
        assert_eq!(
            VerifiedRecord::verify(&bytes),
            Err(RecordError::Malformed("relay url must use https"))
        );

        // The same record with an https relay verifies.
        r.relay_urls = vec!["https://relay.example.org./".into()];
        let d = digest(&r);
        let sa = signer.sign_blocking(a, &d).unwrap();
        let bytes = encode_raw(&r, key.sign(&d).to_bytes(), vec![sa]);
        let rec = VerifiedRecord::verify(&bytes).unwrap();
        assert_eq!(rec.relay_urls().len(), 1);
    }
}
