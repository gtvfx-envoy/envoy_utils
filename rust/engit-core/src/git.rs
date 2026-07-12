//! Low-level git subprocess wrappers.
//!
//! All functions shell out to the `git` executable and return typed
//! `EngitError` values instead of exposing raw process failures.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{EngitError, Result};
use crate::semver::SemVer;

fn args_to_strings<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .map(|value| value.as_ref().to_string_lossy().into_owned())
        .collect()
}

pub(crate) fn run_git<I, S>(args: I, cwd: Option<&Path>) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args_to_strings(args);
    let mut command = Command::new("git");
    command.args(&args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            EngitError::Git(String::from("'git' executable not found on PATH."))
        } else if let Some(cwd) = cwd {
            EngitError::io(cwd.to_path_buf(), source)
        } else {
            EngitError::Git(source.to_string())
        }
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        return Err(EngitError::git_command(&args, &stdout, &stderr));
    }

    Ok(stdout)
}

/// Return `true` when `cwd` is inside a git repository.
pub fn is_git_repo(cwd: Option<&Path>) -> bool {
    run_git(["rev-parse", "--git-dir"], cwd).is_ok()
}

/// Require that `cwd` is inside a git repository.
pub fn require_git_repo(cwd: Option<&Path>) -> Result<()> {
    if is_git_repo(cwd) {
        Ok(())
    } else {
        Err(EngitError::NotAGitRepo)
    }
}

/// Return the repository root for `cwd`.
pub fn get_repo_root(cwd: Option<&Path>) -> Result<PathBuf> {
    require_git_repo(cwd)?;

    Ok(PathBuf::from(run_git(
        ["rev-parse", "--show-toplevel"],
        cwd,
    )?))
}

/// Return the URL for `remote`, or `None` when the remote does not exist.
pub fn get_remote_url(remote: &str, cwd: Option<&Path>) -> Option<String> {
    run_git(["remote", "get-url", remote], cwd).ok()
}

/// Return the nearest tag reachable from `HEAD`, or `None`.
pub fn get_latest_tag(cwd: Option<&Path>) -> Option<String> {
    run_git(["describe", "--tags", "--abbrev=0"], cwd).ok()
}

/// Return the newest semver tag in the repository, or `None`.
pub fn get_latest_semver_tag(cwd: Option<&Path>) -> Option<SemVer> {
    let raw = run_git(["tag", "--list", "--sort=-version:refname"], cwd).ok()?;

    for line in raw.lines() {
        let tag = line.trim();
        if tag.is_empty() {
            continue;
        }
        if let Ok(version) = SemVer::parse(tag) {
            return Some(version);
        }
    }

    None
}

/// Return all semver tags sorted newest-first as raw tag strings.
pub fn get_sorted_semver_tags(cwd: Option<&Path>) -> Vec<String> {
    let Ok(raw) = run_git(["tag", "--list", "--sort=-version:refname"], cwd) else {
        return Vec::new();
    };

    raw.lines()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .filter(|tag| SemVer::parse(tag).is_ok())
        .map(ToOwned::to_owned)
        .collect()
}

/// Create an annotated tag at `HEAD`.
pub fn create_tag(tag: &str, message: &str, cwd: Option<&Path>) -> Result<()> {
    run_git(["tag", "-a", tag, "-m", message], cwd).map(|_| ())
}

/// Return the annotation body for an annotated tag.
pub fn get_tag_annotation(tag: &str, cwd: Option<&Path>) -> Option<String> {
    let object_type = run_git(["cat-file", "-t", tag], cwd).ok()?;
    if object_type != "tag" {
        return None;
    }

    let ref_name = format!("refs/tags/{tag}");
    let body = run_git(["for-each-ref", &ref_name, "--format=%(contents)"], cwd).ok()?;
    let body = body.trim();

    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// Push a single tag to `remote`.
pub fn push_tag(tag: &str, remote: &str, cwd: Option<&Path>) -> Result<()> {
    run_git(["push", remote, tag], cwd).map(|_| ())
}

/// Push a branch and tag together in one operation.
pub fn push_branch_and_tag(
    tag: &str,
    branch: &str,
    remote: &str,
    cwd: Option<&Path>,
) -> Result<()> {
    run_git(["push", remote, branch, tag], cwd).map(|_| ())
}

/// Return commit subjects between `ref_name` and `HEAD`, newest first.
pub fn get_commits_since(ref_name: &str, cwd: Option<&Path>) -> Vec<String> {
    let range = format!("{ref_name}..HEAD");
    let Ok(raw) = run_git(["log", &range, "--pretty=format:%s", "--no-merges"], cwd) else {
        return Vec::new();
    };

    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Return all commit subjects from the start of history.
pub fn get_all_commits(cwd: Option<&Path>) -> Vec<String> {
    let Ok(raw) = run_git(["log", "--pretty=format:%s"], cwd) else {
        return Vec::new();
    };

    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Return the currently checked-out branch, or `None` for detached `HEAD`.
pub fn get_current_branch(cwd: Option<&Path>) -> Option<String> {
    let result = run_git(["rev-parse", "--abbrev-ref", "HEAD"], cwd).ok()?;

    if result == "HEAD" {
        None
    } else {
        Some(result)
    }
}

/// Return a human-readable ahead / behind summary versus `remote/branch`.
pub fn get_branch_comparison(branch: &str, remote: &str, cwd: Option<&Path>) -> String {
    let upstream = format!("{remote}/{branch}");
    let comparison = format!("{upstream}...HEAD");
    let Ok(raw) = run_git(["rev-list", "--left-right", "--count", &comparison], cwd) else {
        return String::new();
    };

    let parts: Vec<_> = raw.split_whitespace().collect();
    if parts.len() != 2 {
        return String::new();
    }

    let behind = parts[0].parse::<u64>().ok();
    let ahead = parts[1].parse::<u64>().ok();
    let (Some(behind), Some(ahead)) = (behind, ahead) else {
        return String::new();
    };

    let mut pieces = Vec::new();
    if ahead > 0 {
        pieces.push(format!("ahead {ahead}"));
    }
    if behind > 0 {
        pieces.push(format!("behind {behind}"));
    }

    pieces.join(", ")
}

/// Return the subject and short SHA of the most recent commit.
pub fn get_last_commit_summary(cwd: Option<&Path>) -> String {
    run_git(["log", "--oneline", "-1"], cwd).unwrap_or_default()
}

/// Return `true` if `tag` exists in the local repository.
pub fn tag_exists(tag: &str, cwd: Option<&Path>) -> bool {
    run_git(["tag", "--list", tag], cwd)
        .map(|result| !result.trim().is_empty())
        .unwrap_or(false)
}

/// Run `git pull` and return the output.
pub fn pull(remote: &str, rebase: bool, cwd: Option<&Path>) -> Result<String> {
    if rebase {
        run_git(["pull", "--rebase", remote], cwd)
    } else {
        run_git(["pull", remote], cwd)
    }
}

/// Return local branches merged into the current branch.
pub fn get_merged_branches(cwd: Option<&Path>) -> Vec<String> {
    let Ok(raw) = run_git(["branch", "--merged"], cwd) else {
        return Vec::new();
    };

    raw.lines()
        .map(|line| line.trim_start_matches('*').trim())
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Delete a local branch.
pub fn delete_local_branch(branch: &str, force: bool, cwd: Option<&Path>) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };

    run_git(["branch", flag, branch], cwd).map(|_| ())
}

/// Prune stale remote-tracking refs from `remote`.
pub fn prune_remote(remote: &str, cwd: Option<&Path>) -> Result<()> {
    run_git(["remote", "prune", remote], cwd).map(|_| ())
}

/// Return the remote-tracking branch for `branch`, or `None` if unset.
pub fn get_remote_tracking_branch(branch: &str, cwd: Option<&Path>) -> Option<String> {
    let remote_key = format!("branch.{branch}.remote");
    let merge_key = format!("branch.{branch}.merge");
    let remote = run_git(["config", &remote_key], cwd).ok()?;
    let mut merge = run_git(["config", &merge_key], cwd).ok()?;

    if let Some(stripped) = merge.strip_prefix("refs/heads/") {
        merge = stripped.to_string();
    }

    Some(format!("{remote}/{merge}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{
        create_tag, get_all_commits, get_commits_since, get_current_branch, get_latest_semver_tag,
        get_repo_root, get_sorted_semver_tags, get_tag_annotation, is_git_repo, require_git_repo,
        tag_exists,
    };
    use crate::error::EngitError;

    fn run_git_raw(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git should be available for tests");

        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).expect("failed to write test file");
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempdir().expect("failed to create temp dir");
        run_git_raw(&["init"], temp.path());
        run_git_raw(&["config", "user.name", "Engit Test"], temp.path());
        run_git_raw(&["config", "user.email", "engit@example.com"], temp.path());

        let file_path = temp.path().join("file.txt");
        write_file(&file_path, "first\n");
        run_git_raw(&["add", "."], temp.path());
        run_git_raw(&["commit", "-m", "Initial commit"], temp.path());

        temp
    }

    #[test]
    fn detects_git_repositories_and_root() {
        let temp = init_repo();
        let non_repo = tempdir().expect("failed to create non-repo temp dir");

        assert!(is_git_repo(Some(temp.path())));
        assert_eq!(
            get_repo_root(Some(temp.path())).expect("repo root should resolve"),
            temp.path()
        );
        assert!(matches!(
            require_git_repo(Some(non_repo.path())),
            Err(EngitError::NotAGitRepo)
        ));
    }

    #[test]
    fn finds_semver_tags_and_annotations() {
        let temp = init_repo();
        create_tag("v1.0.0", "Release v1.0.0", Some(temp.path()))
            .expect("tag creation should succeed");
        run_git_raw(&["tag", "non-semver"], temp.path());
        create_tag(
            "v1.1.0-alpha.1",
            "Release v1.1.0-alpha.1",
            Some(temp.path()),
        )
        .expect("tag creation should succeed");

        let latest =
            get_latest_semver_tag(Some(temp.path())).expect("latest semver tag should resolve");

        assert_eq!(latest.to_tag(), "v1.1.0-alpha.1");
        assert_eq!(
            get_sorted_semver_tags(Some(temp.path())),
            vec![String::from("v1.1.0-alpha.1"), String::from("v1.0.0")]
        );
        assert_eq!(
            get_tag_annotation("v1.0.0", Some(temp.path())),
            Some(String::from("Release v1.0.0"))
        );
        assert!(tag_exists("v1.0.0", Some(temp.path())));
        assert!(!tag_exists("v9.9.9", Some(temp.path())));
    }

    #[test]
    fn returns_commit_subjects_since_ref() {
        let temp = init_repo();
        create_tag("v1.0.0", "Release v1.0.0", Some(temp.path()))
            .expect("tag creation should succeed");

        let file_path = temp.path().join("file.txt");
        write_file(&file_path, "second\n");
        run_git_raw(&["add", "."], temp.path());
        run_git_raw(&["commit", "-m", "Second commit"], temp.path());

        assert_eq!(
            get_commits_since("v1.0.0", Some(temp.path())),
            vec![String::from("Second commit")]
        );
        assert_eq!(get_all_commits(Some(temp.path())).len(), 2);
        assert!(get_current_branch(Some(temp.path())).is_some());
    }
}
