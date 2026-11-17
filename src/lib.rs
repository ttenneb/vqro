//! prim: one lean, standardized layer of workspace primitives over any multiplexer and agent runtime.
//! See `docs/` (start with README.md and decisions D14-D16).

pub mod adapter;
pub mod client;
pub mod core;
pub mod events;
pub mod ext;
pub mod paths;
pub mod provider_client;
pub mod queue;
pub mod limits;
pub mod provider;
pub mod rpc;
pub mod server;
pub mod store;
pub mod tui;
pub mod tmux;
