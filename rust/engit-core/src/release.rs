//! GitHub release creation from a git tag.

use std::path::Path;

use crate::error::{EngitError, Result};
use crate::git::{
    get_commits_since, get_current_branch, get_latest_semver_tag, get_sorted_semver_tags,
    get_tag_annotation, push_branch_and_tag, push_tag, require_git_repo,
};
use crate::github::{create_release, get_release_url, release_exists};
use crate::semver::SemVer;

pub(crate) fn build_draft_notes(_tag: &str, commits: &[String], initial: bool) -> String {
    if !commits.is_empty() {
        return commits
            .iter()
            .map(|message| format!("- {message}"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    if initial {
        return String::from("This is the initial release.");
    }
    String::from("This is a no-change release.")
}

pub(crate) fn parse_annotation(annotation: &str) -> (String, String) {
    let mut lines: Vec<&str> = annotation
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();

    while matches!(lines.first(), Some(line) if line.trim().is_empty()) {
        lines.remove(0);
    }

    if lines.is_empty() {
        return (String::new(), String::new());
    }

    let title = lines[0].trim().to_string();
    let mut body_lines = lines[1..].to_vec();
    while matches!(body_lines.first(), Some(line) if line.trim().is_empty()) {
        body_lines.remove(0);
    }

    (title, body_lines.join("\n").trim().to_string())
}

/// Push the local tag and create a GitHub release.
#[allow(clippy::too_many_arguments)]
pub fn run_release(
    tag: Option<&str>,
    title: Option<&str>,
    draft: bool,
    remote: &str,
    print_only: bool,
    dry_run: bool,
    generate_notes: bool,
    cwd: Option<&Path>,
) -> Result<()> {
    require_git_repo(cwd)?;

    let tag = match tag {
        Some(tag) => tag.to_string(),
        None => {
            let latest = get_latest_semver_tag(cwd).ok_or_else(|| {
                EngitError::NoTagsFound(String::from(
                    "No semantic version tags found locally. Run 'engit tag' \
first, or supply --tag explicitly.",
                ))
            })?;
            latest.to_tag()
        }
    };

    let mut annotation = get_tag_annotation(&tag, cwd);
    if annotation.as_deref().unwrap_or("").is_empty() {
        let semver_tags = get_sorted_semver_tags(cwd);
        let prev_tag = semver_tags
            .iter()
            .position(|candidate| candidate == &tag)
            .and_then(|index| semver_tags.get(index + 1))
            .cloned();
        let commits = prev_tag
            .as_deref()
            .map_or_else(Vec::new, |tag| get_commits_since(tag, cwd));
        let body = build_draft_notes(&tag, &commits, prev_tag.is_none());
        annotation = Some(format!("Release {tag}\n\n{body}"));
    }

    let (parsed_title, notes) = parse_annotation(annotation.as_deref().unwrap_or_default());
    let release_title = title.map(ToOwned::to_owned).unwrap_or_else(|| {
        if parsed_title.is_empty() {
            tag.clone()
        } else {
            parsed_title
        }
    });
    let is_prerelease = SemVer::parse(&tag)
        .ok()
        .and_then(|version| version.prerelease)
        .is_some();

    if print_only {
        println!("Tag:   {tag}");
        println!("Title: {release_title}");
        println!();
        println!("{notes}");
        return Ok(());
    }

    if dry_run {
        println!("\n[dry-run] Would create GitHub release:");
        println!("  Tag:             {tag}");
        println!("  Title:           {release_title}");
        println!("  Remote:          {remote}");
        println!("  Draft:           {draft}");
        println!("  Prerelease:      {is_prerelease}");
        println!("  Generate notes:  {generate_notes}");
        println!();
        println!("--- Notes ---");
        println!("{notes}");
        println!("--- End ---");
        return Ok(());
    }

    if release_exists(&tag)? {
        let existing_url = get_release_url(&tag)?.unwrap_or_else(|| String::from("None"));
        println!("A release for {tag} already exists: {existing_url}");
        return Ok(());
    }

    if let Some(branch) = get_current_branch(cwd) {
        push_branch_and_tag(&tag, &branch, remote, cwd)?;
        println!("Pushed {branch} and {tag} to {remote}");
    } else {
        push_tag(&tag, remote, cwd)?;
        println!("Pushed {tag} to {remote} (detached HEAD — branch not pushed)");
    }

    let url = create_release(
        &tag,
        &release_title,
        &notes,
        draft,
        is_prerelease,
        true,
        generate_notes,
    )?;
    println!("Release created: {url}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_draft_notes, parse_annotation};

    #[test]
    fn builds_initial_and_no_change_notes() {
        assert_eq!(
            build_draft_notes("v1.0.0", &[], true),
            "This is the initial release."
        );
        assert_eq!(
            build_draft_notes("v1.0.1", &[], false),
            "This is a no-change release."
        );
    }

    #[test]
    fn parses_annotation_title_and_body() {
        let annotation = "# comment\nRelease v1.2.3\n\n- One\n- Two\n";
        let (title, body) = parse_annotation(annotation);

        assert_eq!(title, "Release v1.2.3");
        assert_eq!(body, "- One\n- Two");
    }
}
