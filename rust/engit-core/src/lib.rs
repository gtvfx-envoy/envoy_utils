//! `engit-core` -- pure Rust port of engit's non-CLI logic.
//!
//! This crate ports `py/engit/_*.py` module-for-module and depends on the
//! versioned `envoy-core` framework contract. It has no Python runtime
//! dependency. The companion `engit-cli` crate provides the native
//! command-line interface.

pub mod changelog;
pub mod cleanup;
pub mod editor;
pub mod error;
pub mod framework;
pub mod git;
pub mod github;
pub mod publish;
pub mod pull;
pub mod release;
pub mod search;
pub mod semver;
pub mod status;
pub mod tag;
pub mod web;

pub use error::{EngitError, Result};
pub use semver::SemVer;

#[cfg(test)]
pub(crate) static ENVOY_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
