//! Peer-to-peer communication between yagna nodes, built on iroh.
//!
//! Nodes are identified by Ethereum addresses ([`NodeId`]), exactly as in yagna.
//! A node's identities are bound to its iroh endpoint key by a [`record`]
//! that every identity and the endpoint key sign. [`discovery`] maps node IDs
//! to verified records.

#![warn(missing_docs)]

#[cfg(feature = "directory-server")]
pub mod directory;
pub mod discovery;
pub mod identity;
pub mod record;
pub mod signer;
pub mod store;

pub use iroh;

pub use crate::{
    discovery::{Discovery, DiscoveryError, MemoryDiscovery, MultiDiscovery},
    identity::{NodeId, Signature},
    record::{NodeRecord, RecordError, VerifiedRecord},
    signer::{InMemorySigner, SignError, Signer},
    store::RecordStore,
};

#[cfg(feature = "http-directory")]
pub use crate::discovery::HttpDirectory;
