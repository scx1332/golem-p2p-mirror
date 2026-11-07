//! Peer-to-peer communication between yagna nodes, built on iroh.
//!
//! Nodes are identified by Ethereum addresses ([`NodeId`]), exactly as in yagna.

#![warn(missing_docs)]

pub mod identity;
pub mod signer;

pub use crate::{
    identity::{NodeId, Signature},
    signer::{InMemorySigner, SignError, Signer},
};
