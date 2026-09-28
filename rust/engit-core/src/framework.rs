//! Envoy framework integration and stack publishing owned by Engit.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use envoy_core::stack::Stack;
use envoy_core::stack_registry::is_stack_name;

use crate::error::{EngitError, Result};

/// Preferred canonical stack publish root environment variable.
pub const STACK_PUBLISH_ROOT_VAR: &str = "ENVOY_STACK_PUBLISH_ROOT";

const TIMESTAMP_FORMAT: &str = "%Y-%m-%d-%H%M%S";

fn default_stack_root_from_env() -> Result<PathBuf> {
    if let Some(root) = env::var_os(STACK_PUBLISH_ROOT_VAR).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(root));
    }

    Err(EngitError::Framework(format!(
        "No --output specified and {STACK_PUBLISH_ROOT_VAR} is not set."
    )))
}

fn current_timestamp() -> String {
    Utc::now().format(TIMESTAMP_FORMAT).to_string()
}

fn cleanup_failed_publish(version_dir: &Path) {
    let _ = fs::remove_dir_all(version_dir);
}

fn publish_stack_at(
    stack_root: &Path,
    source: &Path,
    dry_run: bool,
    timestamp: &str,
) -> Result<PathBuf> {
    if !source.is_file() {
        return Err(EngitError::Validation(format!(
            "Source stack file does not exist: {}",
            source.display()
        )));
    }

    let stack = Stack::new(source).map_err(|error| EngitError::Framework(error.to_string()))?;
    let source_path = stack.path();
    let name = source_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| is_stack_name(value))
        .ok_or_else(|| {
            EngitError::Validation(format!(
                "Stack filename must contain a valid stack name: {}",
                source_path.display()
            ))
        })?;
    let parent_name = source_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str());
    if parent_name != Some(name) {
        return Err(EngitError::Validation(format!(
            "Stack source parent directory must match filename stem {name:?}: {}",
            source_path.display()
        )));
    }

    let name_dir = stack_root.join(name);
    let version_dir = name_dir.join(timestamp);
    let destination = version_dir.join(format!("{name}.estack"));

    if dry_run {
        println!("Would publish: {}", source_path.display());
        println!("          to: {}", destination.display());
        return Ok(destination);
    }

    fs::create_dir_all(&name_dir).map_err(|source| EngitError::io(&name_dir, source))?;
    fs::create_dir(&version_dir).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            EngitError::Publish(format!(
                "Stack version already exists and is immutable: {}",
                version_dir.display()
            ))
        } else {
            EngitError::io(&version_dir, source)
        }
    })?;

    let staged_destination = version_dir.join(format!(".{name}.estack.{}.tmp", std::process::id()));
    if let Err(source) = fs::copy(source_path, &staged_destination) {
        let _ = fs::remove_file(&staged_destination);
        cleanup_failed_publish(&version_dir);
        return Err(EngitError::io(&destination, source));
    }
    if let Err(source) = fs::rename(&staged_destination, &destination) {
        let _ = fs::remove_file(&staged_destination);
        cleanup_failed_publish(&version_dir);
        return Err(EngitError::io(&destination, source));
    }

    Ok(destination)
}

/// Publish a stack using its filename as the registry name.
pub fn run_publish_stack(
    stack_root: Option<&Path>,
    source: &Path,
    dry_run: bool,
) -> Result<PathBuf> {
    let stack_root = match stack_root {
        Some(path) => path.to_path_buf(),
        None => default_stack_root_from_env()?,
    };

    publish_stack_at(&stack_root, source, dry_run, &current_timestamp())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{
        current_timestamp, default_stack_root_from_env, publish_stack_at, run_publish_stack,
        STACK_PUBLISH_ROOT_VAR,
    };
    use crate::ENVOY_ENV_MUTEX;
    use envoy_core::stack::Stack;
    use regex::Regex;

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

    fn write_stack_source(root: &Path, name: &str) -> PathBuf {
        let bundle = root.join(format!("{name}-bundle"));
        fs::create_dir_all(bundle.join(".envoy")).expect("failed to create bundle");
        let source_dir = root.join(name);
        fs::create_dir_all(&source_dir).expect("failed to create stack source directory");
        let source = source_dir.join(format!("{name}.estack"));
        let contents = format!("bundles:\n  - path: '{}'\n", bundle.display());
        fs::write(&source, &contents).expect("failed to write source stack");
        // Main and the release-prepared dependency pin span the Stack name schema change.
        if Stack::new(&source).is_err() {
            let legacy_contents = format!("name: {name}\n{contents}");
            fs::write(&source, legacy_contents).expect("failed to write legacy source stack");
        }
        source
    }

    #[test]
    fn publishes_stack_to_immutable_version_directory() {
        let temp = tempdir().expect("failed to create temp dir");
        let source = write_stack_source(temp.path(), "studio");

        let stack_root = temp.path().join("stacks");
        let name_dir = stack_root.join("studio");
        let published = publish_stack_at(&stack_root, &source, false, "2026-08-01-152345")
            .unwrap_or_else(|error| panic!("stack should publish: {error}"));

        assert!(published.is_file());
        assert_eq!(
            published,
            name_dir.join("2026-08-01-152345").join("studio.estack")
        );
        assert_eq!(
            fs::read_to_string(&published).expect("published file should be readable"),
            fs::read_to_string(&source).expect("source file should be readable")
        );
    }

    #[test]
    fn publish_leaves_no_staging_artifacts_behind() {
        let temp = tempdir().expect("failed to create temp dir");
        let source = write_stack_source(temp.path(), "studio");
        let stack_root = temp.path().join("stacks");

        publish_stack_at(&stack_root, &source, false, "2026-08-01-152345")
            .unwrap_or_else(|error| panic!("stack should publish: {error}"));

        let version_dir = stack_root.join("studio").join("2026-08-01-152345");
        let entries: Vec<_> = fs::read_dir(&version_dir)
            .expect("version directory should be readable")
            .map(|entry| {
                entry
                    .expect("directory entry should be readable")
                    .file_name()
            })
            .collect();

        assert_eq!(
            entries,
            vec![OsStr::new("studio.estack").to_os_string()],
            "version directory should contain only the published file, no staging artifacts"
        );
    }

    #[test]
    fn current_timestamp_uses_year_month_day_dash_time_format() {
        let timestamp = current_timestamp();
        let pattern = Regex::new(r"^\d{4}-\d{2}-\d{2}-\d{6}$").expect("regex should compile");

        assert!(
            pattern.is_match(&timestamp),
            "unexpected timestamp format: {timestamp}"
        );
    }

    #[test]
    fn publish_requires_source_parent_to_match_filename() {
        let temp = tempdir().expect("failed to create temp dir");
        let valid_source = write_stack_source(temp.path(), "studio");
        let wrong_dir = temp.path().join("custom");
        fs::create_dir_all(&wrong_dir).expect("failed to create mismatched directory");
        let source = wrong_dir.join("studio.estack");
        fs::copy(valid_source, &source).expect("failed to copy stack fixture");

        let error = run_publish_stack(Some(&temp.path().join("stacks")), &source, false)
            .expect_err("mismatched source layout should fail");

        assert!(error.to_string().contains("parent directory must match"));
    }

    #[test]
    fn dry_run_validates_without_writing() {
        let temp = tempdir().expect("failed to create temp dir");
        let source = write_stack_source(temp.path(), "studio");
        let stack_root = temp.path().join("stacks");

        let destination = publish_stack_at(&stack_root, &source, true, "2026-08-01-152345")
            .expect("dry run should succeed");

        assert_eq!(
            destination,
            stack_root
                .join("studio")
                .join("2026-08-01-152345")
                .join("studio.estack")
        );
        assert!(!stack_root.exists());
    }

    #[test]
    fn stack_publish_root_resolves_from_canonical_environment_variable() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let preferred = temp.path().join("preferred");
        let _preferred_guard =
            EnvVarGuard::set(STACK_PUBLISH_ROOT_VAR, Some(preferred.as_os_str()));
        let _legacy_guard = EnvVarGuard::set("ENVOY_STACK_ROOTS", None);

        assert_eq!(
            default_stack_root_from_env().expect("publish root should resolve"),
            preferred
        );
    }

    #[test]
    fn stack_publish_root_ignores_legacy_runtime_roots_variable() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let legacy_roots =
            std::env::join_paths([temp.path().join("legacy")]).expect("failed to join stack roots");
        let _preferred_guard = EnvVarGuard::set(STACK_PUBLISH_ROOT_VAR, None);
        let _legacy_guard = EnvVarGuard::set("ENVOY_STACK_ROOTS", Some(legacy_roots.as_os_str()));

        let error = default_stack_root_from_env()
            .expect_err("legacy ENVOY_STACK_ROOTS must not be used for publishing");

        assert!(error.to_string().contains(STACK_PUBLISH_ROOT_VAR));
    }
}
