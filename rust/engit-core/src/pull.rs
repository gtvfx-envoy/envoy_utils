//! Pull one or more envoy bundle checkouts.

use std::path::{Path, PathBuf};

use envoy_core::discovery::{discover_bundles_auto, Bundle};

use crate::error::{EngitError, Result};
use crate::git::{is_git_repo, pull as git_pull};

pub(crate) fn resolve_specs(specs: &[String]) -> Result<Vec<(String, PathBuf)>> {
    if specs.len() == 1 && specs[0] == "*" {
        let bundles = discover_bundles_auto()?;
        if bundles.is_empty() {
            return Err(EngitError::Engit(String::from(
                "No bundles discovered. Is ENVOY_BNDL_ROOTS set and pointing \
to bundle checkouts?",
            )));
        }

        return Ok(bundles
            .into_iter()
            .map(|bundle| (bundle.bndlid(), bundle.root))
            .collect());
    }

    let mut pairs = Vec::new();
    for spec in specs {
        let bundle = Bundle::new(Path::new(spec), None)?;
        pairs.push((bundle.bndlid(), bundle.path().to_path_buf()));
    }

    Ok(pairs)
}

/// Pull one or more envoy bundle checkouts.
pub fn run_pull(specs: &[String], remote: &str, rebase: bool, dry_run: bool) -> Result<()> {
    let bundles = resolve_specs(specs)?;
    let multi = bundles.len() > 1;

    if dry_run {
        let action = format!("git pull{} {remote}", if rebase { " --rebase" } else { "" });
        if multi {
            println!("Would pull {} bundle(s) [{action}]:", bundles.len());
            for (bndlid, path) in &bundles {
                println!("  {bndlid:<20}  {}", path.display());
            }
        } else if let Some((bndlid, path)) = bundles.first() {
            println!("Would pull {bndlid}  ({})", path.display());
            println!("  Command: {action}");
        }
        return Ok(());
    }

    if multi {
        println!("Pulling {} bundle(s)...", bundles.len());
    }

    let mut failures = Vec::new();
    let width = bundles
        .iter()
        .map(|(bndlid, _)| bndlid.len())
        .max()
        .unwrap_or(0)
        + 2;

    for (bndlid, path) in &bundles {
        if !is_git_repo(Some(path)) {
            let message = "skipped (not a git repo)";
            if multi {
                println!("  {bndlid:<width$} ⚠  {message}");
            } else {
                eprintln!("{bndlid}: {message}");
            }
            continue;
        }

        if multi {
            print!("  {bndlid:<width$}");
        } else {
            println!("Pulling {bndlid}  ({})", path.display());
        }

        match git_pull(remote, rebase, Some(path)) {
            Ok(output) => {
                let first_line = output
                    .lines()
                    .next()
                    .filter(|line| !line.trim().is_empty())
                    .unwrap_or("Done.");
                if multi {
                    println!(" ✓  {first_line}");
                } else if output.trim().is_empty() {
                    println!("Already up to date.");
                } else {
                    println!("{output}");
                }
            }
            Err(error) => {
                let first_line = error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if multi {
                    println!(" ✗  {first_line}");
                    failures.push((bndlid.clone(), error.to_string()));
                } else {
                    return Err(EngitError::Engit(error.to_string()));
                }
            }
        }
    }

    if multi {
        let succeeded = bundles.len().saturating_sub(failures.len());
        println!("\nDone. {succeeded} succeeded, {} failed.", failures.len());
        if !failures.is_empty() {
            println!("  FAILED:");
            for (bndlid, error) in failures {
                let first_line = error.lines().next().unwrap_or_default();
                println!("    {bndlid}: {first_line}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;

    use tempfile::tempdir;

    use super::resolve_specs;
    use crate::BUNDLE_ROOTS_ENV_MUTEX;

    struct EnvVarGuard {
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os("ENVOY_BNDL_ROOTS");
            match value {
                Some(value) => std::env::set_var("ENVOY_BNDL_ROOTS", value),
                None => std::env::remove_var("ENVOY_BNDL_ROOTS"),
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("ENVOY_BNDL_ROOTS", value),
                None => std::env::remove_var("ENVOY_BNDL_ROOTS"),
            }
        }
    }

    #[test]
    fn resolves_explicit_and_wildcard_bundle_specs() {
        let _lock = BUNDLE_ROOTS_ENV_MUTEX
            .lock()
            .expect("bundle roots env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let bundle_root = temp.path().join("bundles");
        let bundle = bundle_root.join("gt").join("pythoncore");
        fs::create_dir_all(bundle.join(".envoy")).expect("failed to create bundle .envoy");
        fs::create_dir_all(bundle.join(".git")).expect("failed to create bundle .git");

        let joined = std::env::join_paths([bundle_root.as_path()]).expect("failed to join paths");
        let _env_guard = EnvVarGuard::set(Some(joined.as_os_str()));

        let explicit = resolve_specs(&[String::from("gt:pythoncore")])
            .expect("explicit bundle should resolve");
        let wildcard = resolve_specs(&[String::from("*")]).expect("wildcard should resolve");

        assert_eq!(explicit[0].0, "gt:pythoncore");
        assert_eq!(
            explicit[0]
                .1
                .canonicalize()
                .expect("explicit bundle path should canonicalize"),
            bundle
                .canonicalize()
                .expect("expected bundle path should canonicalize")
        );
        assert_eq!(wildcard[0].0, "gt:pythoncore");
    }
}
