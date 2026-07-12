//! Merged / stale branch cleanup helpers.

use std::path::Path;

use crate::git::{
    delete_local_branch, get_current_branch, get_merged_branches, get_remote_tracking_branch,
    prune_remote, require_git_repo, run_git,
};

/// Branch names never deleted by cleanup.
pub const PROTECTED_BRANCHES: &[&str] = &["main", "master", "develop"];

/// Clean up merged and stale local branches.
pub fn run_cleanup(remote: &str, noop: bool, cwd: Option<&Path>) -> crate::error::Result<()> {
    require_git_repo(cwd)?;

    let current = get_current_branch(cwd).unwrap_or_default();

    if noop {
        let _ = run_git(["remote", "prune", remote, "--dry-run"], cwd);
    } else if let Err(error) = prune_remote(remote, cwd) {
        println!("Warning: could not prune remote \"{remote}\": {error}");
    } else {
        println!("Pruned stale remote-tracking refs for {remote}.");
    }

    let mut deleted = Vec::new();
    let mut skipped = Vec::new();

    for branch in get_merged_branches(cwd) {
        if branch == current || PROTECTED_BRANCHES.contains(&branch.as_str()) {
            continue;
        }
        if noop {
            println!("  Would delete {branch} [merged]");
            continue;
        }
        match delete_local_branch(&branch, false, cwd) {
            Ok(()) => {
                deleted.push(branch.clone());
                println!("  Deleted {branch} [merged]");
            }
            Err(error) => {
                skipped.push(branch.clone());
                println!("  Skipped {branch}: {error}");
            }
        }
    }

    let raw = run_git(["branch"], cwd).unwrap_or_default();
    for line in raw.lines() {
        let branch = line.trim_start_matches('*').trim();
        if branch.is_empty()
            || branch == current
            || PROTECTED_BRANCHES.contains(&branch)
            || deleted
                .iter()
                .any(|deleted_branch| deleted_branch == branch)
        {
            continue;
        }

        let Some(tracking) = get_remote_tracking_branch(branch, cwd) else {
            continue;
        };
        let ref_name = format!("refs/remotes/{tracking}");
        if run_git(["show-ref", "--verify", "--quiet", &ref_name], cwd).is_ok() {
            continue;
        }

        if noop {
            println!("  Would delete {branch} [remote deleted]");
            continue;
        }

        match delete_local_branch(branch, true, cwd) {
            Ok(()) => {
                deleted.push(branch.to_string());
                println!("  Deleted {branch} [remote deleted]");
            }
            Err(error) => {
                skipped.push(branch.to_string());
                println!("  Skipped {branch}: {error}");
            }
        }
    }

    if !noop {
        if deleted.is_empty() {
            println!("No local branches to clean up.");
        } else {
            println!("\nDeleted {} branch(es).", deleted.len());
            println!("Use `git branch <name> <revision>` or `git reflog` to restore if needed.");
        }
        if !skipped.is_empty() {
            println!(
                "Warning: {} branch(es) could not be deleted: {}",
                skipped.len(),
                skipped.join(", ")
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PROTECTED_BRANCHES;

    #[test]
    fn protected_branch_list_matches_python_defaults() {
        assert_eq!(PROTECTED_BRANCHES, &["main", "master", "develop"]);
    }
}
