//! Peer-to-peer communication between yagna nodes, built on [iroh].
//!
//! Nodes are identified by Ethereum addresses ([`NodeId`]), exactly as in
//! yagna. Transport, encryption and hole punching come from iroh. A node's
//! Ethereum identities are bound to its iroh endpoint key by a [`record`]
//! that every identity and the endpoint key sign, so relays and directories
//! never have to be trusted with identity information.
//!
//! All traffic is encrypted end to end with iroh's QUIC/TLS 1.3, always: there
//! is no plaintext mode and no opt-out. Relay URLs must use `https`.
//!
//! See `docs/design.md` in the repository for the full design.
//!
//! ```no_run
//! # async fn example() -> golem_p2p::Result<()> {
//! use golem_p2p::{Endpoint, Identities, InMemorySigner, MemoryDiscovery};
//!
//! let discovery = MemoryDiscovery::new();
//! let signer = InMemorySigner::new();
//! let id = signer.generate();
//! let ep = Endpoint::builder(signer, Identities::new(id))
//!     .relay("https://relay.example.org".parse().unwrap())
//!     .discovery(discovery.clone())
//!     .bind()
//!     .await?;
//!
//! // Server side: answer requests, each in its own task (`respond` waits
//! // until the peer has read the response, and one failed request must not
//! // stop the server).
//! let server = ep.clone();
//! tokio::spawn(async move {
//!     while let Some(mut incoming) = server.accept().await {
//!         tokio::spawn(async move {
//!             let Ok(req) = incoming.read_request(1024).await else {
//!                 return;
//!             };
//!             println!("request from {} to {}", incoming.from(), incoming.to());
//!             let _ = incoming.respond(&req).await;
//!         });
//!     }
//! });
//!
//! // Client side.
//! # let other: golem_p2p::NodeId = "0x00a329c0648769a73afac7f9381e08fb43dbea72".parse().unwrap();
//! let reply = ep.request(other, b"ping").await?;
//! # Ok(()) }
//! ```

#![warn(missing_docs)]

mod addrs;
#[cfg(feature = "directory-server")]
pub mod directory;
pub mod discovery;
mod endpoint;
mod error;
pub mod identity;
pub mod lookup;
pub mod record;
pub mod signer;
pub mod store;
pub mod wire;

pub use iroh;
pub use iroh::endpoint::{RecvStream, SendStream};

pub use crate::{
    discovery::{Discovery, DiscoveryError, MemoryDiscovery, MultiDiscovery},
    endpoint::{
        Builder, Connection, Direction, Endpoint, Event, Identities, IncomingStream, PathKind,
        TransportMode,
    },
    error::{Error, Result},
    identity::{NodeId, Signature},
    record::{NodeRecord, RecordError, VerifiedRecord},
    signer::{InMemorySigner, SignError, Signer},
    store::RecordStore,
};

#[cfg(feature = "http-directory")]
pub use crate::discovery::HttpDirectory;
