//! Error type of the endpoint API.

use crate::{
    discovery::DiscoveryError,
    identity::NodeId,
    record::RecordError,
    wire::{FrameError, RejectReason},
};

/// Errors returned by [`crate::Endpoint`] and [`crate::Connection`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// No valid record is known for this node ID.
    #[error("no valid record found for node {0}")]
    NotFound(NodeId),
    /// The identity is not one of this endpoint's identities.
    #[error("{0} is not an identity of this endpoint")]
    NotLocalIdentity(NodeId),
    /// The target is one of this endpoint's own identities.
    #[error("refusing to connect to own identity {0}")]
    SelfConnect(NodeId),
    /// The remote peer's current record does not list this identity.
    #[error("peer does not serve identity {0}")]
    NotPeerIdentity(NodeId),
    /// An identity update would leave the endpoint without identities, or is invalid.
    #[error("invalid identity set: {0}")]
    InvalidIdentities(&'static str),
    /// Creating or verifying a record failed.
    #[error(transparent)]
    Record(#[from] RecordError),
    /// The discovery service failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// A configured relay URL does not use `https`. Relay connections always
    /// use TLS; plain `http` relays are not supported.
    #[error("relay URL {0} is not allowed: relay URLs must use https")]
    InsecureRelayUrl(String),
    /// Binding the iroh endpoint failed.
    #[error("bind failed: {0}")]
    Bind(String),
    /// Establishing the QUIC connection failed.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The golem-p2p handshake failed.
    #[error("handshake failed: {0}")]
    Handshake(String),
    /// The remote endpoint presented a record for a different iroh endpoint.
    #[error("peer record does not match the authenticated connection")]
    IdentityMismatch,
    /// The remote refused the handshake.
    #[error("peer rejected the connection: {0}")]
    Rejected(RejectReason),
    /// The remote refused an application stream (QUIC error code).
    #[error("peer rejected the stream (code {0})")]
    StreamRejected(u64),
    /// A connection or stream failed.
    #[error("connection error: {0}")]
    Connection(String),
    /// A frame could not be read or written.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// A message exceeded the size limit.
    #[error("message too large")]
    TooLarge,
    /// An operation timed out.
    #[error("timed out")]
    Timeout,
    /// The endpoint has been closed.
    #[error("endpoint closed")]
    Closed,
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
