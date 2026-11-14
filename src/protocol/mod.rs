//! Shared wire protocol and presentation encoding code.

pub(crate) mod collapse;
pub mod endpoint;
pub mod endpoint_bridge_v1;
pub(crate) mod render_ansi;
pub(crate) mod scene;
mod wire;

pub use wire::*;
