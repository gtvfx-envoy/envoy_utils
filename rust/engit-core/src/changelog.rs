//! Changelog generation from GitHub releases.

use std::path::Path;

use serde::Deserialize;

use crate::error::{EngitError, Result};
use crate::git::require_git_repo;
use crate::github::{parse_release_summary, run_gh};
use crate::semver::SemVer;

#[derive(Clone, Debug, Deserialize)]
struct ReleaseDetail {
    #[serde(rename = "tagName")]
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
}

fn fetch_release_detail(tag: &str) -> Result<ReleaseDetail> {
    let raw = run_gh(
        &["release", "view", tag, "--json", "tagName,name,body"],
        None,
    )?;

    serde_json::from_str(&raw)
        .map_err(|source| EngitError::GitHub(format!("Unexpected response from gh: {source}")))
}

fn sort_releases(mut releases: Vec<ReleaseDetail>) -> Vec<ReleaseDetail> {
    releases.retain(|release| SemVer::parse(&release.tag_name).is_ok());
    releases.sort_by(|left, right| {
        let left = SemVer::parse(&left.tag_name).expect("release tag already validated");
        let right = SemVer::parse(&right.tag_name).expect("release tag already validated");

        right.cmp(&left)
    });
    releases
}

/// Print a changelog generated from GitHub releases.
pub fn run_changelog(tag: Option<&str>, cwd: Option<&Path>) -> Result<()> {
    require_git_repo(cwd)?;

    let releases = if let Some(tag) = tag {
        vec![fetch_release_detail(tag)?]
    } else {
        let raw = run_gh(
            &[
                "release",
                "list",
                "--limit",
                "100",
                "--json",
                "tagName,name,isPrerelease",
            ],
            None,
        )?;
        let summaries = parse_release_summary(&raw)?;
        let mut releases = Vec::new();

        for summary in summaries {
            if let Ok(detail) = fetch_release_detail(&summary.tag_name) {
                releases.push(detail);
            }
        }

        sort_releases(releases)
    };

    if releases.is_empty() {
        println!("No releases found.");
        return Ok(());
    }

    if tag.is_none() {
        println!("# Release notes\n");
    }

    for release in releases {
        let title = release.name.unwrap_or_else(|| release.tag_name.clone());
        let body = release.body.unwrap_or_default();
        println!("## {title}");
        if !body.trim().is_empty() {
            println!();
            println!("{}", body.trim());
        }
        println!();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{sort_releases, ReleaseDetail};

    #[test]
    fn sorts_valid_semver_releases_newest_first() {
        let releases = vec![
            ReleaseDetail {
                tag_name: String::from("v1.0.0-alpha.1"),
                name: None,
                body: None,
            },
            ReleaseDetail {
                tag_name: String::from("v1.0.0"),
                name: None,
                body: None,
            },
            ReleaseDetail {
                tag_name: String::from("not-a-version"),
                name: None,
                body: None,
            },
        ];

        let sorted = sort_releases(releases);

        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].tag_name, "v1.0.0");
        assert_eq!(sorted[1].tag_name, "v1.0.0-alpha.1");
    }
}
