//! Ethereum-style node identities, compatible with yagna.
//!
//! A node identity is a secp256k1 key pair. The [`NodeId`] is the 20-byte
//! Ethereum address of the public key: the last 20 bytes of
//! `keccak256(uncompressed_public_key[1..])`. This is the same derivation as
//! `ya_client_model::NodeId` / `ethsign::PublicKey::address`.
//!
//! Signatures use the yagna wire format: 65 bytes `v || r || s`, where `v` is
//! the raw recovery id (0 or 1), computed over a 32-byte prehash with no
//! Ethereum message prefix. This is what yagna's identity service returns for
//! `identity::Sign` and what `ya-relay` sends in its challenge responses.

use std::{fmt, str::FromStr};

use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

/// A 20-byte Ethereum address identifying a node identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId([u8; 20]);

impl NodeId {
    /// Wraps raw address bytes.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Returns the raw address bytes.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Derives the node ID from a secp256k1 public key.
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let point = key.to_sec1_point(false);
        let bytes = point.as_bytes();
        // Uncompressed SEC1: 0x04 || X (32) || Y (32). Hash X || Y.
        debug_assert_eq!(bytes.len(), 65);
        let hash = Keccak256::digest(&bytes[1..]);
        let mut out = [0u8; 20];
        out.copy_from_slice(&hash[12..]);
        Self(out)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}

/// Error parsing a [`NodeId`] from a string.
#[derive(Debug, thiserror::Error)]
#[error("invalid node id: expected 0x followed by 40 hex characters")]
pub struct ParseNodeIdError;

impl FromStr for NodeId {
    type Err = ParseNodeIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        if s.len() != 40 {
            return Err(ParseNodeIdError);
        }
        let mut out = [0u8; 20];
        hex::decode_to_slice(s, &mut out).map_err(|_| ParseNodeIdError)?;
        Ok(Self(out))
    }
}

/// A 65-byte recoverable secp256k1 signature in yagna format (`v || r || s`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; 65]);

/// Error decoding or verifying a [`Signature`].
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SignatureError {
    /// The signature is not 65 bytes long.
    #[error("invalid signature length {0}, expected 65")]
    Length(usize),
    /// The recovery id byte is not 0, 1, 27 or 28.
    #[error("invalid recovery id {0}")]
    RecoveryId(u8),
    /// The `r`/`s` values do not form a valid signature, or recovery failed.
    #[error("malformed signature or public key recovery failed")]
    Malformed,
}

impl Signature {
    /// Length of the encoded signature.
    pub const LENGTH: usize = 65;

    /// Wraps raw `v || r || s` bytes. The contents are checked on recovery.
    pub const fn from_bytes(bytes: [u8; 65]) -> Self {
        Self(bytes)
    }

    /// Parses a signature from a slice, checking only the length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, SignatureError> {
        let arr: [u8; 65] = bytes
            .try_into()
            .map_err(|_| SignatureError::Length(bytes.len()))?;
        Ok(Self(arr))
    }

    /// Returns the raw `v || r || s` bytes.
    pub const fn as_bytes(&self) -> &[u8; 65] {
        &self.0
    }

    /// Recovers the public key that produced this signature over `prehash`.
    ///
    /// Accepts `v` as a raw recovery id (0/1, the yagna format) or with the
    /// legacy Ethereum offset (27/28). High-`s` signatures are rejected.
    pub fn recover(&self, prehash: &[u8; 32]) -> Result<VerifyingKey, SignatureError> {
        let v = match self.0[0] {
            v @ (0 | 1) => v,
            v @ (27 | 28) => v - 27,
            v => return Err(SignatureError::RecoveryId(v)),
        };
        let recid = RecoveryId::from_byte(v).ok_or(SignatureError::RecoveryId(v))?;
        let sig =
            EcdsaSignature::from_slice(&self.0[1..]).map_err(|_| SignatureError::Malformed)?;
        if sig.normalize_s() != sig {
            return Err(SignatureError::Malformed);
        }
        VerifyingKey::recover_from_prehash(prehash, &sig, recid)
            .map_err(|_| SignatureError::Malformed)
    }

    /// Recovers the [`NodeId`] that produced this signature over `prehash`.
    pub fn recover_node_id(&self, prehash: &[u8; 32]) -> Result<NodeId, SignatureError> {
        self.recover(prehash).map(|k| NodeId::from_public_key(&k))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature(0x{})", hex::encode(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "4d5db4107d237df6a3d58ee5f70ae63d73d7658d4026f2eefd2f204c81682cb7";

    /// Test vector from ethsign 0.9 (the library yagna uses): secret key -> address.
    #[test]
    fn node_id_matches_ethsign_vector() {
        let secret = hex::decode(SECRET).unwrap();
        let key = k256::ecdsa::SigningKey::from_slice(&secret).unwrap();
        let id = NodeId::from_public_key(key.verifying_key());
        assert_eq!(id.to_string(), "0x00a329c0648769a73afac7f9381e08fb43dbea72");
    }

    /// Signature vector from ethsign 0.9: the key signs its own 32-byte secret
    /// as the message. Both libraries use RFC 6979 nonces, so the signature
    /// must match byte for byte, in yagna's `v || r || s` layout.
    #[test]
    fn signature_matches_ethsign_vector() {
        let secret: [u8; 32] = hex::decode(SECRET).unwrap().try_into().unwrap();
        let signer = crate::signer::InMemorySigner::new();
        let id = signer.add_secret(&secret).unwrap();
        let sig = signer.sign_blocking(id, &secret).unwrap();
        let expected = concat!(
            "00",
            "8a4f2d73a2cc80cdfe27c6e3ab68de7913865a5968298731bee7b4673752fd76",
            "77c9027a03e635b730b3e3e593f968d0ef7cad1848cf0293be9d7aba56c71859"
        );
        assert_eq!(hex::encode(sig.as_bytes()), expected);
        assert_eq!(sig.recover_node_id(&secret).unwrap(), id);
    }

    #[test]
    fn sign_recover_roundtrip() {
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let prehash = [42u8; 32];
        let (sig, recid) = key.sign_prehash_recoverable(&prehash);
        let mut bytes = [0u8; 65];
        bytes[0] = recid.to_byte();
        bytes[1..].copy_from_slice(&sig.to_bytes());
        let sig = Signature::from_bytes(bytes);
        let id = sig.recover_node_id(&prehash).unwrap();
        assert_eq!(id, NodeId::from_public_key(key.verifying_key()));

        // Legacy 27/28 offset is accepted.
        let mut legacy = bytes;
        legacy[0] += 27;
        assert_eq!(
            Signature::from_bytes(legacy)
                .recover_node_id(&prehash)
                .unwrap(),
            id
        );

        // Garbage recovery id is rejected.
        let mut bad = bytes;
        bad[0] = 5;
        assert_eq!(
            Signature::from_bytes(bad).recover_node_id(&prehash),
            Err(SignatureError::RecoveryId(5))
        );

        // A different message recovers a different identity.
        assert_ne!(sig.recover_node_id(&[0u8; 32]).ok(), Some(id));
    }

    #[test]
    fn node_id_parse_display() {
        let s = "0x005b3bcf82085eededd551f50de7892471ffb272";
        let id: NodeId = s.parse().unwrap();
        assert_eq!(id.to_string(), s);
        assert_eq!(
            "005B3BCF82085EEDEDD551F50DE7892471FFB272"
                .parse::<NodeId>()
                .unwrap(),
            id
        );
        assert!("0x1234".parse::<NodeId>().is_err());
    }
}
