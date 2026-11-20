//! The renderer-independent client model (research `declarations-and-ui-clients.md`, D29): what every vqro client
//! needs whatever it draws with. The ratatui client (`tui`) is a renderer over it; clients in other languages
//! implement the same behaviour against the conformance fixtures in `tests/fixtures/client/`.
//!
//! - `workspace`: the snapshot/event cache and contributions (visible menus and bindings, binding precedence).
//! - `panel`: panel flattening (pages, tree, filter, toolbar), selection steps, `requires` and element fallbacks.
//! - `view`: view-kind fallbacks.
//! - `markdown`: formatted text rows (`format: "markdown"`).
//! - `composer`: the composer row and multi-line prompts (editing, draft, controls).
//! - `recovery`: the shared recovery content (prefix recovery rows and the safe-mode chooser).
//! - `keys`: key names.
//! - `links`: URLs and file paths in terminal rows (Ctrl-click, the pane menu's link).
//! - `tabbar`: tab bar labels from the shared `doc/settings/tabs` setting (chips, number style).
//! - `window`: which layout (workspace) a window shows, its invocation context and `current_when` marks.
//! - this module: client profiles and UI features declared in `hello`.
//!
//! Nothing here imports a terminal or rendering library.

pub mod composer;
pub mod keys;
pub mod links;
pub mod markdown;
pub mod panel;
pub mod recovery;
pub mod tabbar;
pub mod view;
pub mod window;
pub mod workspace;

use serde_json::{json, Value};

/// Profiles the built-in client implements: `base` (every interactive client, including recovery), `terminal`
/// (terminal views) and `keys` (prefix, bindings, `client.*` commands).
pub const PROFILES: &[&str] = &["base", "terminal", "keys"];

/// `hello` parameters for a rendering client: its name, version, build, profiles and the UI features it renders.
pub fn hello(client: &str, build: Option<u64>, profiles: &[&str], features: &[&str]) -> Value {
    json!({"client": client, "version": env!("CARGO_PKG_VERSION"), "build": build, "profiles": profiles, "features": features})
}
