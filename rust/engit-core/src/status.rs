//! Repository status summary helpers.

use std::path::Path;

use crate::error::Result;
use crate::git::{
    get_branch_comparison, get_current_branch, get_last_commit_summary, get_latest_semver_tag,
    require_git_repo,
};

pub(crate) fn format_status_lines(
    branch: &str,
    comparison: &str,
    latest_tag: &str,
    last_commit: &str,
) -> [String; 3] {
    let branch_line = if comparison.is_empty() {
        branch.to_string()
    } else {
        format!("{branch} [{comparison}]")
    };
    let width = 16;

    [
        format!("{:<width$}{}", "Branch:", branch_line),
        format!("{:<width$}{}", "Last tag:", latest_tag),
        format!("{:<width$}{}", "Last commit:", last_commit),
    ]
}

/// Print a one-screen status summary for the current repository.
pub fn run_status(remote: &str, cwd: Option<&Path>) -> Result<()> {
    require_git_repo(cwd)?;

    let branch = get_current_branch(cwd).unwrap_or_else(|| String::from("(detached HEAD)"));
    let comparison = if branch == "(detached HEAD)" {
        String::new()
    } else {
        get_branch_comparison(&branch, remote, cwd)
    };
    let latest_tag = get_latest_semver_tag(cwd)
        .map(|tag| tag.to_tag())
        .unwrap_or_else(|| String::from("(none)"));
    let last_commit = {
        let summary = get_last_commit_summary(cwd);
        if summary.is_empty() {
            String::from("(no commits)")
        } else {
            summary
        }
    };

    for line in format_status_lines(&branch, &comparison, &latest_tag, &last_commit) {
        println!("{line}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::format_status_lines;

    #[test]
    fn formats_status_summary_lines() {
        let lines = format_status_lines("main", "ahead 2", "v1.2.3", "abc123 Commit");

        assert_eq!(lines[0], "Branch:         main [ahead 2]");
        assert_eq!(lines[1], "Last tag:       v1.2.3");
        assert_eq!(lines[2], "Last commit:    abc123 Commit");
    }
}
