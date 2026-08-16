//! Transports drive the sans-io [`Session`](crate::sync::Session) over real
//! connections. First implementation: blocking websockets ([`ws`]).

#[cfg(feature = "ws")]
pub mod ws;
