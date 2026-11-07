//! Peer-to-peer communication between yagna nodes, built on iroh.
//!
//! Nodes are identified by Ethereum addresses ([`NodeId`]), exactly as in yagna.
//! A node's identities are bound to its iroh endpoint key by a [`record`]
//! that every identity and the endpoint key sign.

#![warn(missing_docs)]

pub mod identity;
pub mod record;
pub mod signer;
pub mod store;

pub use iroh;

pub use crate::{
    identity::{NodeId, Signature},
    record::{NodeRecord, RecordError, VerifiedRecord},
    signer::{InMemorySigner, SignError, Signer},
    store::RecordStore,
};
