//! Native Claude Code stream/control adapter. No credential discovery or input replay.
pub mod availability;
pub mod contract;
pub mod launch;
mod plans;
pub mod requests;
pub mod restart;
pub mod runtime;
pub mod startup;
pub mod state;
pub mod wire;
mod work;
