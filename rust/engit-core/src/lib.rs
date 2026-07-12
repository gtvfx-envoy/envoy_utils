//! `engit-core` -- pure Rust port of engit's git/GitHub tooling logic.
//!
//! Placeholder for the Phase 5 port of `py/engit/*.py`. Depends on
//! `envoy-core` for bundle discovery / named-config resolution, matching
//! today's `from envoy._discovery import ...` / `from envoy._config_registry
//! import ...` usage in `py/engit/_pull.py` and `py/engit/_cli.py`.

pub use envoy_core::EnvoyError;
