//! The golem-p2p wire protocol on top of iroh QUIC connections.
//!
//! Every connection uses ALPN [`ALPN`]; an endpoint offers and accepts no
//! other. All traffic is inside iroh's QUIC/TLS 1.3 session, which is
//! authenticated by the iroh endpoint keys and encrypted end to end, also
//! through relays. There is exactly one mode: no unencrypted mode and no option
//! to turn encryption off (see the design document, section 4).
//!
//! **Control stream.** The dialer opens the first bidirectional stream, writes
//! [`KIND_CONTROL`] and a [`Control::Hello`] frame with its signed record and
//! the identity it wants to reach. The acceptor verifies the record, checks
//! that the record's endpoint ID equals the TLS-authenticated remote endpoint
//! ID, and answers [`Control::Welcome`] with its own record (or
//! [`Control::Reject`]). The dialer runs the same checks. The stream stays open
//! and carries [`Control::Update`] frames whenever either side's record
//! changes.
//!
//! **Application streams.** Every further bidirectional stream starts with
//! [`KIND_APP`] and an [`AppHeader`] naming the sending and the receiving
//! identity. The receiver accepts the stream only if `to` is one of its own
//! current identities and `from` is listed in the peer's current verified
//! record. The header is a claim that is checked, never a source of identity.
//!
//! Frames are a 4-byte big-endian length followed by a postcard encoding.

use iroh::endpoint::{RecvStream, SendStream, VarInt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::identity::NodeId;

/// ALPN identifier of protocol version 1.
pub const ALPN: &[u8] = b"golem-p2p/1";

/// First byte of the control stream.
pub const KIND_CONTROL: u8 = 1;
/// First byte of an application stream.
pub const KIND_APP: u8 = 2;

/// Maximum size of a control frame.
pub const MAX_CONTROL_FRAME: usize = 16 * 1024;
/// Maximum size of an application stream header.
pub const MAX_HEADER_FRAME: usize = 256;

/// Messages on the control stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Control {
    /// Dialer to acceptor: my record, and which of your identities I want.
    Hello {
        /// Encoded signed record of the dialer.
        record: Vec<u8>,
        /// Identity the dialer wants to reach, if any.
        target: Option<NodeId>,
    },
    /// Acceptor to dialer: my record.
    Welcome {
        /// Encoded signed record of the acceptor.
        record: Vec<u8>,
    },
    /// Acceptor to dialer: connection refused.
    Reject {
        /// Machine readable reason.
        reason: RejectReason,
    },
    /// Either direction: my record changed.
    Update {
        /// Encoded signed record.
        record: Vec<u8>,
    },
}

/// Why a handshake was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RejectReason {
    /// The requested identity is not (or no longer) served by this node.
    #[error("the requested identity is not served by this node")]
    UnknownIdentity,
    /// The dialer's record did not verify or did not match the connection.
    #[error("invalid or mismatching node record")]
    InvalidRecord,
    /// The acceptor is at its connection limit.
    #[error("too many connections")]
    Busy,
}

/// Header of an application stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppHeader {
    /// Sending identity; must be listed in the sender's verified record.
    pub from: NodeId,
    /// Receiving identity; must be one of the receiver's identities.
    pub to: NodeId,
}

/// QUIC application error codes used on streams and connections.
pub mod codes {
    use super::VarInt;

    /// Normal close.
    pub const OK: VarInt = VarInt::from_u32(0);
    /// Handshake failed or timed out.
    pub const HANDSHAKE_FAILED: VarInt = VarInt::from_u32(1);
    /// The record presented does not belong to the TLS-authenticated endpoint.
    pub const IDENTITY_MISMATCH: VarInt = VarInt::from_u32(2);
    /// A newer instance of the same node replaced this connection.
    pub const SUPERSEDED: VarInt = VarInt::from_u32(3);
    /// The requested identity is not served here.
    pub const UNKNOWN_IDENTITY: VarInt = VarInt::from_u32(4);
    /// Connection limit reached.
    pub const BUSY: VarInt = VarInt::from_u32(5);
    /// Protocol violation.
    pub const PROTOCOL: VarInt = VarInt::from_u32(6);
    /// Stream rejected: the sender identity is not bound to the connection.
    pub const SENDER_NOT_BOUND: VarInt = VarInt::from_u32(7);
    /// The local endpoint is shutting down.
    pub const SHUTDOWN: VarInt = VarInt::from_u32(8);
}

/// Error reading or writing frames.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Stream I/O failed.
    #[error("stream error: {0}")]
    Io(String),
    /// Frame larger than allowed.
    #[error("frame too large ({0} bytes)")]
    TooLarge(usize),
    /// Frame could not be decoded.
    #[error("malformed frame")]
    Malformed,
}

/// Writes one length-prefixed frame.
pub async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<(), FrameError> {
    let body = postcard::to_allocvec(msg).map_err(|_| FrameError::Malformed)?;
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge(body.len()))?;
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))
}

/// Reads one length-prefixed frame of at most `max` bytes.
pub async fn read_frame<T: DeserializeOwned>(
    recv: &mut RecvStream,
    max: usize,
) -> Result<T, FrameError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    postcard::from_bytes(&body).map_err(|_| FrameError::Malformed)
}

/// Reads the single stream-kind byte.
pub async fn read_kind(recv: &mut RecvStream) -> Result<u8, FrameError> {
    let mut kind = [0u8; 1];
    recv.read_exact(&mut kind)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    Ok(kind[0])
}
