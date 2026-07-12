//! Semantic version tag creation helpers.

use std::path::Path;

use crate::editor::open_in_editor;
use crate::error::{EngitError, Result};
use crate::git::{
    create_tag, get_commits_since, get_latest_semver_tag, get_sorted_semver_tags, require_git_repo,
    tag_exists,
};
use crate::semver::SemVer;

fn next_prerelease_number(base_ver: &SemVer, label: &str, cwd: Option<&Path>) -> u64 {
    let prefix = format!("{}-{label}.", base_ver.to_tag());
    let mut existing = Vec::new();

    for tag in get_sorted_semver_tags(cwd) {
        if let Some(suffix) = tag.strip_prefix(&prefix) {
            if let Ok(number) = suffix.parse::<u64>() {
                existing.push(number);
            }
        }
    }

    existing.into_iter().max().map_or(1, |number| number + 1)
}

/// Resolve the next version to tag.
pub fn resolve_next_version(
    bump: Option<&str>,
    version: Option<&str>,
    cwd: Option<&Path>,
) -> Result<SemVer> {
    if bump.is_some() == version.is_some() {
        return Err(EngitError::Validation(String::from(
            "Provide exactly one of 'bump' or 'version'.",
        )));
    }

    if let Some(version) = version {
        let parsed = SemVer::parse(version)?;
        if parsed.prerelease.is_some() && parsed.prerelease_number().is_none() {
            let label = parsed
                .prerelease_label()
                .expect("prerelease label must exist when prerelease exists");
            let base = SemVer {
                major: parsed.major,
                minor: parsed.minor,
                patch: parsed.patch,
                prerelease: None,
            };
            let next_number = next_prerelease_number(&base, label, cwd);

            return Ok(SemVer {
                major: parsed.major,
                minor: parsed.minor,
                patch: parsed.patch,
                prerelease: Some(format!("{label}.{next_number}")),
            });
        }

        return Ok(parsed);
    }

    let current = get_latest_semver_tag(cwd).ok_or_else(|| {
        EngitError::NoTagsFound(String::from(
            "No semantic version tags found in this repository. Use --version \
to supply an explicit first version (e.g. --version 0.0.1).",
        ))
    })?;

    match bump
        .expect("bump must exist when version is absent")
        .to_ascii_lowercase()
        .as_str()
    {
        "major" => Ok(current.bump_major()),
        "minor" => Ok(current.bump_minor()),
        "patch" => Ok(current.bump_patch()),
        bump => Err(EngitError::Validation(format!(
            "Unknown bump component '{bump}'. Use 'major', 'minor', or 'patch'."
        ))),
    }
}

pub(crate) fn strip_comments(text: &str) -> String {
    let mut kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();

    while matches!(kept.first(), Some(line) if line.trim().is_empty()) {
        kept.remove(0);
    }
    while matches!(kept.last(), Some(line) if line.trim().is_empty()) {
        kept.pop();
    }

    kept.join("\n")
}

pub(crate) fn build_tag_draft(
    tag: &str,
    default_annotation: &str,
    commits: &[String],
    prev_tag: Option<&str>,
) -> String {
    let body_lines: Vec<String> = if !commits.is_empty() {
        commits
            .iter()
            .map(|message| format!("- {message}"))
            .collect()
    } else if prev_tag.is_none() {
        vec![String::from("This is the initial release.")]
    } else {
        vec![String::from("This is a no-change release.")]
    };

    let mut lines = vec![default_annotation.to_string(), String::new()];
    lines.extend(body_lines);
    lines.extend([
        String::new(),
        String::from("#"),
        format!("# Write a message for tag: {tag}"),
    ]);

    if let Some(prev_tag) = prev_tag {
        lines.push(format!("# Previous tag: {prev_tag}"));
        lines.push(format!(
            "# Commits above pre-populated from git log since {prev_tag}."
        ));
    } else {
        lines.push(String::from("# First tag in this repository."));
    }

    lines.push(String::from("# Lines starting with '#' will be ignored."));
    lines.push(String::from(
        "# Save the file to confirm. Close without saving to cancel.",
    ));

    format!("{}\n", lines.join("\n"))
}

/// Create a local annotated git tag for the next semantic version.
pub fn run_tag(
    bump: Option<&str>,
    version: Option<&str>,
    message: Option<&str>,
    print_only: bool,
    dry_run: bool,
    cwd: Option<&Path>,
) -> Result<Option<SemVer>> {
    require_git_repo(cwd)?;

    let next_version = resolve_next_version(bump, version, cwd)?;
    let tag_name = next_version.to_tag();
    let default_annotation = message
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("Release {tag_name}"));

    if print_only {
        println!("{tag_name}");
        return Ok(Some(next_version));
    }

    if dry_run {
        println!("[dry-run] Would create tag: {tag_name}");
        return Ok(Some(next_version));
    }

    if tag_exists(&tag_name, cwd) {
        return Err(EngitError::Git(format!(
            "Tag '{tag_name}' already exists. Use --version to supply a \
different version."
        )));
    }

    let semver_tags = get_sorted_semver_tags(cwd);
    let prev_tag = semver_tags.first().map(String::as_str);
    let commits = prev_tag.map_or_else(Vec::new, |tag| get_commits_since(tag, cwd));

    let annotation = if let Some(message) = message {
        strip_comments(message)
    } else {
        let draft = build_tag_draft(&tag_name, &default_annotation, &commits, prev_tag);
        let Some(raw) = open_in_editor(&draft, "TAG_EDITMSG")? else {
            println!("Tag aborted.");
            return Ok(None);
        };
        strip_comments(&raw)
    };

    if annotation.trim().is_empty() {
        println!("Tag aborted: empty annotation after removing comments.");
        return Ok(None);
    }

    create_tag(&tag_name, &annotation, cwd)?;
    println!("Created tag: {tag_name}");
    println!("Run 'engit release' when ready to publish.");

    Ok(Some(next_version))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{build_tag_draft, resolve_next_version, strip_comments};
    use crate::error::EngitError;

    fn run_git(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git should be available");

        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempdir().expect("failed to create temp dir");
        run_git(&["init"], temp.path());
        run_git(&["config", "user.name", "Engit Test"], temp.path());
        run_git(&["config", "user.email", "engit@example.com"], temp.path());
        fs::write(temp.path().join("file.txt"), "one\n").expect("failed to write file");
        run_git(&["add", "."], temp.path());
        run_git(&["commit", "-m", "Initial commit"], temp.path());
        temp
    }

    #[test]
    fn resolves_explicit_prerelease_with_auto_number() {
        let temp = init_repo();
        run_git(&["tag", "v1.2.3-alpha.1"], temp.path());
        run_git(&["tag", "v1.2.3-alpha.3"], temp.path());

        let version = resolve_next_version(None, Some("1.2.3-alpha"), Some(temp.path()))
            .expect("version should resolve");

        assert_eq!(version.to_tag(), "v1.2.3-alpha.4");
    }

    #[test]
    fn bump_requires_existing_tags() {
        let temp = init_repo();
        let error = resolve_next_version(Some("patch"), None, Some(temp.path()))
            .expect_err("bump without tags should fail");

        assert!(matches!(error, EngitError::NoTagsFound(_)));
    }

    #[test]
    fn strips_comment_lines_and_blank_edges() {
        assert_eq!(
            strip_comments("\n# comment\nTitle\n\nBody\n# trailing\n"),
            "Title\n\nBody"
        );
    }

    #[test]
    fn build_tag_draft_includes_initial_release_message() {
        let draft = build_tag_draft("v1.0.0", "Release v1.0.0", &[], None);

        assert!(draft.contains("This is the initial release."));
        assert!(draft.contains("# First tag in this repository."));
    }
}
