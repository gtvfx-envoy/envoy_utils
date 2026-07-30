//! Narrow adapter around the Envoy framework APIs used by Engit.

use std::env;
use std::path::{Path, PathBuf};

use envoy_core::stack_registry::{publish_stack, STACK_ROOTS_VAR};

use crate::error::{EngitError, Result};

/// Preferred canonical stack publish root environment variable.
pub const STACK_PUBLISH_ROOT_VAR: &str = "ENVOY_STACK_PUBLISH_ROOT";

fn default_stack_root_from_env() -> Result<PathBuf> {
    if let Some(root) = env::var_os(STACK_PUBLISH_ROOT_VAR).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(root));
    }

    let separator = if cfg!(windows) { ';' } else { ':' };
    let legacy_root = env::var(STACK_ROOTS_VAR).ok().and_then(|raw| {
        raw.split(separator)
            .map(str::trim)
            .find(|entry| !entry.is_empty())
            .map(PathBuf::from)
    });
    if let Some(root) = legacy_root {
        eprintln!(
            "warning: using the first {STACK_ROOTS_VAR} entry for publishing is \
deprecated; use {STACK_PUBLISH_ROOT_VAR} instead."
        );
        return Ok(root);
    }

    Err(EngitError::Framework(format!(
        "No --output specified and neither {STACK_PUBLISH_ROOT_VAR} nor \
{STACK_ROOTS_VAR} is set."
    )))
}

/// Publish a named stack through Envoy's stack registry contract.
pub fn run_publish_stack(
    stack_root: Option<&Path>,
    name: &str,
    source: &Path,
    dry_run: bool,
) -> Result<PathBuf> {
    let stack_root = match stack_root {
        Some(path) => path.to_path_buf(),
        None => default_stack_root_from_env()?,
    };

    publish_stack(&stack_root, name, source, dry_run)
        .map_err(|error| EngitError::Framework(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;

    use tempfile::tempdir;

    use super::{default_stack_root_from_env, run_publish_stack, STACK_PUBLISH_ROOT_VAR};
    use crate::ENVOY_ENV_MUTEX;
    use envoy_core::stack_registry::STACK_ROOTS_VAR;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn publishes_stack_to_explicit_root() {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(bundle.join(".envoy")).expect("failed to create bundle");

        let source = temp.path().join("studio.estack");
        let contents = format!("name: studio\nbundles:\n  - path: '{}'\n", bundle.display());
        fs::write(&source, contents).expect("failed to write source stack");

        let stack_root = temp.path().join("stacks");
        let published = run_publish_stack(Some(&stack_root), "studio", &source, false)
            .expect("stack should publish");

        assert!(published.is_file());
        assert!(stack_root.join("studio").join("latest").is_file());
    }

    #[test]
    fn stack_publish_root_prefers_canonical_environment_variable() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let preferred = temp.path().join("preferred");
        let legacy = temp.path().join("legacy");
        let legacy_roots = std::env::join_paths([legacy]).expect("failed to join stack roots");
        let _preferred_guard =
            EnvVarGuard::set(STACK_PUBLISH_ROOT_VAR, Some(preferred.as_os_str()));
        let _legacy_guard = EnvVarGuard::set(STACK_ROOTS_VAR, Some(legacy_roots.as_os_str()));

        assert_eq!(
            default_stack_root_from_env().expect("publish root should resolve"),
            preferred
        );
    }

    #[test]
    fn stack_publish_root_falls_back_to_first_runtime_root() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let legacy_roots = std::env::join_paths([first.as_path(), second.as_path()])
            .expect("failed to join roots");
        let _preferred_guard = EnvVarGuard::set(STACK_PUBLISH_ROOT_VAR, None);
        let _legacy_guard = EnvVarGuard::set(STACK_ROOTS_VAR, Some(legacy_roots.as_os_str()));

        assert_eq!(
            default_stack_root_from_env().expect("legacy publish root should resolve"),
            first
        );
    }
}
