//! Standalone tmux backend library; no provider protocol or vqro core dependency.
mod client;
pub mod osc;
pub mod parser;
pub use client::{Client, Config, Error, GapReason, PaneEvent, PaneStream};
pub use parser::{Notification, PaneId};
