//! Standalone tmux backend library; no provider protocol or prim core dependency.
pub mod parser;
pub mod osc;
pub use parser::{Notification, PaneId};
