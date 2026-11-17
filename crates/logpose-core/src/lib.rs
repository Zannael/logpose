//! Logpose core library.
//!
//! Everything here is computed locally at zero token cost and has no GUI dependencies, so it
//! builds and tests without the Tauri/webkit stack.

pub mod agent_rule;
pub mod board;
pub mod config;
pub mod digest;
pub mod discovery;
pub mod git;
pub mod launcher;
pub mod mtime;
pub mod next_md;
pub mod packer;
pub mod platform;
pub mod removal;
pub mod runner;
pub mod scan;
pub mod score;
pub mod sessions;
pub mod stats;
pub mod store;
pub mod synth;
pub mod synthesis;
pub mod usage;
pub mod watch;

/// Crate version, exposed for diagnostics and the `logpose` CLI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
