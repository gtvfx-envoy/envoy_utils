//! Envoy framework integration and stack publishing owned by Engit.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use envoy_core::stack::Stack;
use envoy_core::stack_registry::{is_stack_name, STACK_ROOTS_VAR};

use crate::error::{EngitError, Result};

/// Preferred canonical stack publish root environment variable.
pub const STACK_PUBLISH_ROOT_VAR: &str = "ENVOY_STACK_PUBLISH_ROOT";

const LATEST_LINK: &str = "latest.estack";
const LEGACY_LATEST_POINTER: &str = "latest";
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H-%M-%S";

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

fn current_timestamp() -> String {
    Utc::now().format(TIMESTAMP_FORMAT).to_string()
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn cleanup_failed_publish(version_dir: &Path, temporary_link: Option<&Path>) {
    if let Some(temporary_link) = temporary_link {
        let _ = fs::remove_file(temporary_link);
    }
    let _ = fs::remove_dir_all(version_dir);
}

fn publish_stack_at<F>(
    stack_root: &Path,
    source: &Path,
    dry_run: bool,
    timestamp: &str,
    create_symlink: F,
) -> Result<PathBuf>
where
    F: Fn(&Path, &Path) -> io::Result<()>,
{
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
    let latest_link = name_dir.join(LATEST_LINK);
    let relative_target = Path::new(timestamp).join(format!("{name}.estack"));

    if dry_run {
        println!("Would publish: {}", source_path.display());
        println!("          to: {}", destination.display());
        println!(
            "      latest: {} -> {}",
            latest_link.display(),
            relative_target.display()
        );
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

    if let Err(source) = fs::copy(source_path, &destination) {
        cleanup_failed_publish(&version_dir, None);
        return Err(EngitError::io(&destination, source));
    }

    let temporary_link = name_dir.join(format!(".{LATEST_LINK}.{}.tmp", std::process::id()));
    if fs::symlink_metadata(&temporary_link).is_ok() {
        cleanup_failed_publish(&version_dir, Some(&temporary_link));
        return Err(EngitError::Publish(format!(
            "Temporary latest link already exists: {}",
            temporary_link.display()
        )));
    }
    if let Err(source) = create_symlink(&relative_target, &temporary_link) {
        cleanup_failed_publish(&version_dir, Some(&temporary_link));
        return Err(EngitError::io(&temporary_link, source));
    }

    let backup_link = name_dir.join(format!(".{LATEST_LINK}.{}.backup", std::process::id()));
    let had_previous = fs::symlink_metadata(&latest_link).is_ok();
    if had_previous {
        if fs::symlink_metadata(&backup_link).is_ok() {
            cleanup_failed_publish(&version_dir, Some(&temporary_link));
            return Err(EngitError::Publish(format!(
                "Temporary latest backup already exists: {}",
                backup_link.display()
            )));
        }
        if let Err(source) = fs::rename(&latest_link, &backup_link) {
            cleanup_failed_publish(&version_dir, Some(&temporary_link));
            return Err(EngitError::io(&latest_link, source));
        }
    }

    if let Err(source) = fs::rename(&temporary_link, &latest_link) {
        if had_previous {
            let _ = fs::rename(&backup_link, &latest_link);
        }
        cleanup_failed_publish(&version_dir, Some(&temporary_link));
        return Err(EngitError::io(&latest_link, source));
    }

    if had_previous {
        let _ = fs::remove_file(&backup_link);
    }
    let legacy_pointer = name_dir.join(LEGACY_LATEST_POINTER);
    if fs::symlink_metadata(&legacy_pointer).is_ok() {
        let _ = fs::remove_file(legacy_pointer);
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

    publish_stack_at(
        &stack_root,
        source,
        dry_run,
        &current_timestamp(),
        create_file_symlink,
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{
        create_file_symlink, default_stack_root_from_env, publish_stack_at, run_publish_stack,
        LATEST_LINK, LEGACY_LATEST_POINTER, STACK_PUBLISH_ROOT_VAR,
    };
    use crate::{EngitError, ENVOY_ENV_MUTEX};
    use envoy_core::stack::Stack;
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
    fn publishes_stack_to_nested_version_and_updates_latest_symlink() {
        let temp = tempdir().expect("failed to create temp dir");
        let source = write_stack_source(temp.path(), "studio");

        let stack_root = temp.path().join("stacks");
        let name_dir = stack_root.join("studio");
        fs::create_dir_all(&name_dir).expect("failed to create stack name directory");
        fs::write(name_dir.join(LEGACY_LATEST_POINTER), "legacy.estack")
            .expect("failed to write legacy pointer");
        let published = match publish_stack_at(
            &stack_root,
            &source,
            false,
            "2026-08-01T15-23-45",
            create_file_symlink,
        ) {
            Ok(published) => published,
            Err(EngitError::Io { source, .. }) if source.raw_os_error() == Some(1314) => return,
            Err(error) => panic!("stack should publish: {error}"),
        };

        assert!(published.is_file());
        assert_eq!(
            published,
            name_dir.join("2026-08-01T15-23-45").join("studio.estack")
        );
        let latest_link = name_dir.join(LATEST_LINK);
        assert_eq!(
            fs::read_link(&latest_link).expect("latest should be a symlink"),
            Path::new("2026-08-01T15-23-45").join("studio.estack")
        );
        assert_eq!(
            fs::canonicalize(latest_link).expect("latest target should resolve"),
            fs::canonicalize(published).expect("published path should resolve")
        );
        assert!(!name_dir.join(LEGACY_LATEST_POINTER).exists());
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

        let destination = publish_stack_at(
            &stack_root,
            &source,
            true,
            "2026-08-01T15-23-45",
            create_file_symlink,
        )
        .expect("dry run should succeed");

        assert_eq!(
            destination,
            stack_root
                .join("studio")
                .join("2026-08-01T15-23-45")
                .join("studio.estack")
        );
        assert!(!stack_root.exists());
    }

    #[test]
    fn symlink_failure_cleans_version_and_preserves_previous_latest() {
        let temp = tempdir().expect("failed to create temp dir");
        let source = write_stack_source(temp.path(), "studio");
        let stack_root = temp.path().join("stacks");
        let name_dir = stack_root.join("studio");
        fs::create_dir_all(&name_dir).expect("failed to create stack name directory");
        let latest_link = name_dir.join(LATEST_LINK);
        fs::write(&latest_link, "previous").expect("failed to write previous latest fixture");

        let error = publish_stack_at(
            &stack_root,
            &source,
            false,
            "2026-08-01T15-23-45",
            |_, _| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
        )
        .expect_err("symlink failure should fail publication");

        assert!(error.to_string().contains("denied"));
        assert_eq!(
            fs::read_to_string(latest_link).expect("previous latest should remain"),
            "previous"
        );
        assert!(!name_dir.join("2026-08-01T15-23-45").exists());
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
