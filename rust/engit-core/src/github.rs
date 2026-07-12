//! GitHub operations via the `gh` CLI.
//!
//! These helpers intentionally shell out to the GitHub CLI rather than using
//! a Rust API crate, matching the Python implementation's behavior.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::error::{EngitError, Result};

/// Single repository search result returned by `gh search repos`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RepoSearchResult {
    /// Repository name only.
    pub name: String,
    /// Full `owner/name` repository name.
    pub full_name: String,
    /// Human-readable description.
    pub description: String,
    /// HTML repository URL.
    pub url: String,
    /// Stargazer count.
    pub stars: u64,
    /// Last update timestamp string from GitHub.
    pub updated_at: String,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Deserialize)]
pub(crate) struct ReleaseSummary {
    #[serde(rename = "tagName")]
    pub(crate) tag_name: String,
}

#[derive(Debug, Deserialize)]
struct RawSearchResult {
    name: Option<String>,
    #[serde(rename = "fullName")]
    full_name: Option<String>,
    description: Option<String>,
    url: Option<String>,
    #[serde(rename = "stargazersCount")]
    stargazers_count: Option<u64>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
}

pub(crate) fn run_gh(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let args_vec: Vec<String> = args.iter().map(|value| (*value).to_string()).collect();
    let mut command = Command::new("gh");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            EngitError::GhCliNotFound
        } else if let Some(cwd) = cwd {
            EngitError::io(cwd.to_path_buf(), source)
        } else {
            EngitError::GitHub(source.to_string())
        }
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        return Err(EngitError::github_command(&args_vec, &stdout, &stderr));
    }

    Ok(stdout)
}

/// Create a GitHub release for an existing tag.
pub fn create_release(
    tag: &str,
    title: &str,
    notes: &str,
    draft: bool,
    prerelease: bool,
    latest: bool,
    generate_notes: bool,
) -> Result<String> {
    let mut args = vec!["release", "create", tag, "--title", title, "--notes", notes];
    if draft {
        args.push("--draft");
    }
    if prerelease {
        args.push("--prerelease");
    }
    if latest && !prerelease {
        args.push("--latest");
    }
    if generate_notes {
        args.push("--generate-notes");
    }

    run_gh(&args, None)
}

/// Return `true` if a GitHub release already exists for `tag`.
pub fn release_exists(tag: &str) -> Result<bool> {
    let output = Command::new("gh")
        .args(["release", "view", tag, "--json", "tagName"])
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                EngitError::GhCliNotFound
            } else {
                EngitError::GitHub(source.to_string())
            }
        })?;

    Ok(output.status.success())
}

/// Return the HTML URL of an existing release, or `None`.
pub fn get_release_url(tag: &str) -> Result<Option<String>> {
    let output = Command::new("gh")
        .args(["release", "view", tag, "--json", "url", "--jq", ".url"])
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                EngitError::GhCliNotFound
            } else {
                EngitError::GitHub(source.to_string())
            }
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stdout))
    }
}

/// Return the GitHub organisation that owns the current repository.
pub fn get_current_org() -> Option<String> {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("gh")
        .args(["repo", "view", "--json", "owner", "--jq", ".owner.login"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// Search repositories matching `query`.
pub fn search_repos(
    query: &str,
    orgs: Option<&[String]>,
    limit: usize,
) -> Result<Vec<RepoSearchResult>> {
    let targets: Vec<Option<&str>> = match orgs {
        Some(orgs) if !orgs.is_empty() => orgs.iter().map(|org| Some(org.as_str())).collect(),
        _ => vec![None],
    };
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for org in targets {
        let limit_string = limit.to_string();
        let mut args = vec![
            "search",
            "repos",
            query,
            "--limit",
            limit_string.as_str(),
            "--json",
            "name,fullName,description,url,stargazersCount,updatedAt",
        ];
        if let Some(org) = org {
            args.splice(3..3, ["--owner", org]);
        }

        let raw = run_gh(&args, None)?;
        if raw.is_empty() {
            continue;
        }

        let parsed = parse_search_results(&raw)?;
        for repo in parsed {
            if seen.insert(repo.full_name.clone()) {
                results.push(repo);
            }
        }
    }

    Ok(results)
}

pub(crate) fn parse_search_results(raw: &str) -> Result<Vec<RepoSearchResult>> {
    let items: Vec<RawSearchResult> = serde_json::from_str(raw).map_err(|source| {
        EngitError::GitHub(format!("Unexpected response from gh search: {source}"))
    })?;

    Ok(items
        .into_iter()
        .map(|item| RepoSearchResult {
            name: item.name.unwrap_or_default(),
            full_name: item.full_name.unwrap_or_default(),
            description: item.description.unwrap_or_default(),
            url: item.url.unwrap_or_default(),
            stars: item.stargazers_count.unwrap_or(0),
            updated_at: item.updated_at.unwrap_or_default(),
        })
        .collect())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_release_summary(raw: &str) -> Result<Vec<ReleaseSummary>> {
    serde_json::from_str(raw)
        .map_err(|source| EngitError::GitHub(format!("Unexpected response from gh: {source}")))
}

#[cfg(test)]
mod tests {
    use super::{create_release, parse_search_results, EngitError, RepoSearchResult};

    #[test]
    fn parses_repo_search_results() {
        let raw = r#"
        [
          {
            "name": "gt-envoy",
            "fullName": "gtvfx-contrib/gt-envoy",
            "description": "Envoy",
            "url": "https://github.com/gtvfx-contrib/gt-envoy",
            "stargazersCount": 42,
            "updatedAt": "2026-07-01T12:00:00Z"
          }
        ]
        "#;

        assert_eq!(
            parse_search_results(raw).expect("search results should parse"),
            vec![RepoSearchResult {
                name: String::from("gt-envoy"),
                full_name: String::from("gtvfx-contrib/gt-envoy"),
                description: String::from("Envoy"),
                url: String::from("https://github.com/gtvfx-contrib/gt-envoy"),
                stars: 42,
                updated_at: String::from("2026-07-01T12:00:00Z"),
            }]
        );
    }

    #[test]
    fn invalid_search_json_returns_github_error() {
        let error = parse_search_results("{").expect_err("invalid json should fail");

        assert!(matches!(error, EngitError::GitHub(_)));
    }

    #[test]
    fn create_release_reports_missing_gh_when_unavailable() {
        let has_gh = std::process::Command::new("gh")
            .arg("--version")
            .output()
            .is_ok();

        if !has_gh {
            let error = create_release(
                "v1.0.0",
                "Release v1.0.0",
                "Notes",
                false,
                false,
                true,
                false,
            )
            .expect_err("missing gh should fail");
            assert!(matches!(error, EngitError::GhCliNotFound));
        }
    }
}
