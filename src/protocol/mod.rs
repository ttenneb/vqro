//! Shared wire protocol and presentation encoding code.

pub mod endpoint;
pub mod endpoint_bridge_v1;
pub(crate) mod render_ansi;
mod wire;

pub use wire::*;
