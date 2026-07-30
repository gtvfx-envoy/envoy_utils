//! Narrow adapter around the Envoy framework APIs used by Engit.

use std::env;
use std::path::{Path, PathBuf};

use envoy_core::stack_registry::{publish_stack, STACK_ROOTS_VAR};

use crate::error::{EngitError, Result};

fn default_stack_root_from_env() -> Result<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };

    env::var(STACK_ROOTS_VAR)
        .ok()
        .and_then(|raw| {
            raw.split(separator)
                .map(str::trim)
                .find(|entry| !entry.is_empty())
                .map(PathBuf::from)
        })
        .ok_or_else(|| {
            EngitError::Framework(format!(
                "No --stack-root specified and {STACK_ROOTS_VAR} is not set."
            ))
        })
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
    use std::fs;

    use tempfile::tempdir;

    use super::run_publish_stack;

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
}
