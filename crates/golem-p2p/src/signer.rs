//! Signing through a trait, so keys can live in an external identity service.
//!
//! In yagna the identity service owns the secp256k1 keys, may keep some of
//! them locked, and signs 32-byte payloads on request (`identity::Sign`). This
//! crate never needs a secret key: it only asks a [`Signer`] to sign the digest
//! of a node record for each identity listed in it.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use k256::ecdsa::SigningKey;

use crate::identity::{NodeId, Signature};

/// A boxed, `Send` future, used to keep [`Signer`] object safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Error returned by a [`Signer`].
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SignError {
    /// The signer does not hold a key for this identity.
    #[error("unknown identity {0}")]
    UnknownIdentity(NodeId),
    /// The key exists but is locked and cannot sign right now.
    #[error("identity {0} is locked")]
    Locked(NodeId),
    /// Any other failure of the signing backend (for example an RPC error).
    #[error("signer failure: {0}")]
    Backend(String),
}

/// Signs 32-byte digests on behalf of node identities.
///
/// Implementations must return a yagna-format signature (`v || r || s`, `v` in
/// `{0, 1}`) over exactly the given 32 bytes, without any message prefix. The
/// returned signature is always verified by recovering the signer's address,
/// so a misbehaving signer cannot produce an invalid record.
pub trait Signer: Send + Sync + 'static {
    /// Signs `digest` with the key of `node_id`.
    fn sign(
        &self,
        node_id: NodeId,
        digest: [u8; 32],
    ) -> BoxFuture<'_, Result<Signature, SignError>>;
}

impl<T: Signer + ?Sized> Signer for Arc<T> {
    fn sign(
        &self,
        node_id: NodeId,
        digest: [u8; 32],
    ) -> BoxFuture<'_, Result<Signature, SignError>> {
        (**self).sign(node_id, digest)
    }
}

struct KeyEntry {
    key: SigningKey,
    locked: bool,
}

/// An in-memory secp256k1 signer for tests, examples and tools.
///
/// Keys can be locked and unlocked to simulate yagna's identity service.
#[derive(Clone, Default)]
pub struct InMemorySigner {
    keys: Arc<Mutex<HashMap<NodeId, KeyEntry>>>,
}

impl std::fmt::Debug for InMemorySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<NodeId> = self
            .keys
            .lock()
            .expect("poisoned")
            .keys()
            .copied()
            .collect();
        f.debug_struct("InMemorySigner")
            .field("identities", &ids)
            .finish()
    }
}

/// Error for an invalid secp256k1 secret key.
#[derive(Debug, thiserror::Error)]
#[error("invalid secp256k1 secret key")]
pub struct InvalidSecretKey;

impl InMemorySigner {
    /// Creates an empty signer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Generates a fresh random identity and returns its node ID.
    pub fn generate(&self) -> NodeId {
        loop {
            let bytes: [u8; 32] = rand::random();
            if let Ok(id) = self.add_secret(&bytes) {
                return id;
            }
        }
    }

    /// Adds an identity from a raw 32-byte secret key and returns its node ID.
    pub fn add_secret(&self, secret: &[u8; 32]) -> Result<NodeId, InvalidSecretKey> {
        let key = SigningKey::from_slice(secret).map_err(|_| InvalidSecretKey)?;
        let id = NodeId::from_public_key(key.verifying_key());
        self.keys
            .lock()
            .expect("poisoned")
            .insert(id, KeyEntry { key, locked: false });
        Ok(id)
    }

    /// Returns the raw secret key of an identity, for persisting test keys.
    pub fn secret(&self, id: NodeId) -> Option<[u8; 32]> {
        let keys = self.keys.lock().expect("poisoned");
        keys.get(&id).map(|e| e.key.to_bytes().into())
    }

    /// Removes an identity from the signer.
    pub fn remove(&self, id: NodeId) -> bool {
        self.keys.lock().expect("poisoned").remove(&id).is_some()
    }

    /// Locks an identity: signing with it fails with [`SignError::Locked`].
    pub fn lock(&self, id: NodeId) -> bool {
        self.set_locked(id, true)
    }

    /// Unlocks a previously locked identity.
    pub fn unlock(&self, id: NodeId) -> bool {
        self.set_locked(id, false)
    }

    fn set_locked(&self, id: NodeId, locked: bool) -> bool {
        match self.keys.lock().expect("poisoned").get_mut(&id) {
            Some(e) => {
                e.locked = locked;
                true
            }
            None => false,
        }
    }

    /// Signs synchronously. Same semantics as [`Signer::sign`].
    pub fn sign_blocking(&self, id: NodeId, digest: &[u8; 32]) -> Result<Signature, SignError> {
        let keys = self.keys.lock().expect("poisoned");
        let entry = keys.get(&id).ok_or(SignError::UnknownIdentity(id))?;
        if entry.locked {
            return Err(SignError::Locked(id));
        }
        let (sig, recid) = entry.key.sign_prehash_recoverable(digest);
        let mut out = [0u8; 65];
        out[0] = recid.to_byte();
        out[1..].copy_from_slice(&sig.to_bytes());
        Ok(Signature::from_bytes(out))
    }
}

impl Signer for InMemorySigner {
    fn sign(
        &self,
        node_id: NodeId,
        digest: [u8; 32],
    ) -> BoxFuture<'_, Result<Signature, SignError>> {
        let res = self.sign_blocking(node_id, &digest);
        Box::pin(async move { res })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_unlock() {
        let s = InMemorySigner::new();
        let id = s.generate();
        let digest = [1u8; 32];
        let sig = s.sign_blocking(id, &digest).unwrap();
        assert_eq!(sig.recover_node_id(&digest).unwrap(), id);
        assert!(s.lock(id));
        assert_eq!(s.sign_blocking(id, &digest), Err(SignError::Locked(id)));
        assert!(s.unlock(id));
        assert!(s.sign_blocking(id, &digest).is_ok());
        let other = NodeId::from_bytes([9; 20]);
        assert_eq!(
            s.sign_blocking(other, &digest),
            Err(SignError::UnknownIdentity(other))
        );
    }
}
