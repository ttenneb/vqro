//! vqro: one lean, standardized layer of workspace primitives over any multiplexer and agent runtime.
//! See `docs/` (start with README.md and decisions D14-D16).

pub mod adapter;
pub mod client;
pub mod transport;
mod commit;
mod invocation;
pub mod core;
pub mod events;
pub mod ext;
mod ext_log;
pub mod paths;
pub mod principal;
pub mod causality;
pub mod provider_client;
pub mod codex;
pub mod queue;
pub mod limits;
pub mod manifest;
pub mod provider;
pub mod program;
pub mod provider_wire;
pub mod bridge_tokens;
pub mod resource_admission;
pub mod rpc;
pub mod server;
pub mod shutdown;
pub mod store;
pub mod operational;
pub mod tui;
pub mod ui_launch;
pub mod ui_model;
pub mod tmux;

#[cfg(test)]
#[path = "../tests/support/timing.rs"]
mod test_timing;
