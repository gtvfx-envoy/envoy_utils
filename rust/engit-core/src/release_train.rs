//! Cross-repo release-train orchestration.
//!
//! Provides a single-pane status view across Envoy's core and downstream
//! repositories, and a way to compute the next downstream version and
//! dispatch its existing, unmodified Prepare Release workflow with the
//! correct inputs -- so a maintainer (or CI, on a `review` compatibility
//! classification) never has to hand-copy version numbers between repos.

use serde::Deserialize;

use crate::error::{EngitError, Result};
use crate::github::run_gh;
use crate::semver::SemVer;

/// Default GitHub organisation for the Envoy release-train repositories.
pub const DEFAULT_OWNER: &str = "gtvfx-envoy";

/// Repositories that participate in the Envoy release train.
pub const TRAIN_REPOS: &[&str] = &["envoy", "envoy_utils", "despatch"];

#[derive(Debug, Deserialize)]
struct LatestReleaseTag {
    #[serde(rename = "tagName")]
    tag_name: String,
}

#[derive(Debug, Deserialize)]
struct IssueLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IssueSummary {
    number: u64,
    title: String,
    labels: Vec<IssueLabel>,
}

fn full_repo(owner: &str, repo: &str) -> String {
    format!("{owner}/{repo}")
}

/// The `dependency:*` label a downstream repo's tracked impact issues use,
/// or `None` for a repo (such as `envoy` itself) that has no such issues.
fn dependency_label(repo: &str) -> Option<&'static str> {
    match repo {
        "envoy_utils" => Some("dependency:envoy-core"),
        "despatch" => Some("dependency:envoy"),
        _ => None,
    }
}

fn latest_release_tag(owner: &str, repo: &str) -> Option<String> {
    let full = full_repo(owner, repo);
    let output = run_gh(
        &[
            "release",
            "view",
            "--repo",
            full.as_str(),
            "--json",
            "tagName",
        ],
        None,
    )
    .ok()?;
    let parsed: LatestReleaseTag = serde_json::from_str(&output).ok()?;
    Some(parsed.tag_name)
}

fn format_issue_summary(issue: &IssueSummary) -> String {
    let impact_labels: Vec<&str> = issue
        .labels
        .iter()
        .map(|label| label.name.as_str())
        .filter(|name| name.starts_with("release-impact:"))
        .collect();
    if impact_labels.is_empty() {
        format!("#{} {}", issue.number, issue.title)
    } else {
        format!(
            "#{} {} [{}]",
            issue.number,
            issue.title,
            impact_labels.join(", ")
        )
    }
}

fn open_impact_issues(owner: &str, repo: &str) -> Result<Vec<String>> {
    let Some(label) = dependency_label(repo) else {
        return Ok(Vec::new());
    };
    let full = full_repo(owner, repo);
    let output = run_gh(
        &[
            "issue",
            "list",
            "--repo",
            full.as_str(),
            "--label",
            label,
            "--state",
            "open",
            "--json",
            "number,title,labels",
        ],
        None,
    )?;
    let issues: Vec<IssueSummary> = serde_json::from_str(&output).map_err(|source| {
        EngitError::ReleaseTrain(format!(
            "Could not parse `gh issue list` output for {full}: {source}"
        ))
    })?;
    Ok(issues.iter().map(format_issue_summary).collect())
}

/// Print a single-pane release-train status: latest release and open
/// impact issues for each requested repository (default: all three).
pub fn run_release_train_status(owner: &str, repos: &[String]) -> Result<()> {
    let targets: Vec<&str> = if repos.is_empty() {
        TRAIN_REPOS.to_vec()
    } else {
        repos.iter().map(String::as_str).collect()
    };

    for repo in targets {
        println!("{repo}");
        match latest_release_tag(owner, repo) {
            Some(tag) => println!("  Latest release: {tag}"),
            None => println!("  Latest release: (none found)"),
        }
        match open_impact_issues(owner, repo) {
            Ok(issues) if issues.is_empty() => println!("  Open impact issues: none"),
            Ok(issues) => {
                println!("  Open impact issues:");
                for issue in issues {
                    println!("    {issue}");
                }
            }
            Err(error) => println!("  Open impact issues: (could not query: {error})"),
        }
    }

    Ok(())
}

/// Compute the next downstream version from an explicit version or a bump
/// of the latest published release tag. Defaults to a patch bump when
/// neither a bump component nor an explicit version is given.
pub(crate) fn resolve_next_downstream_version(
    bump: Option<&str>,
    explicit_version: Option<&str>,
    latest_tag: Option<&str>,
) -> Result<SemVer> {
    if let Some(version) = explicit_version {
        if bump.is_some() {
            return Err(EngitError::Validation(String::from(
                "Provide --version or a bump flag (--major/--minor/--patch), not both.",
            )));
        }
        return SemVer::parse(version);
    }

    let current = match latest_tag {
        Some(tag) => SemVer::parse(tag)?,
        None => {
            return Err(EngitError::ReleaseTrain(String::from(
                "No existing release found for this repository; supply an \
explicit --version for a first release.",
            )));
        }
    };

    match bump.unwrap_or("patch").to_ascii_lowercase().as_str() {
        "major" => Ok(current.bump_major()),
        "minor" => Ok(current.bump_minor()),
        "patch" => Ok(current.bump_patch()),
        other => Err(EngitError::Validation(format!(
            "Unknown bump component '{other}'. Use 'major', 'minor', or 'patch'."
        ))),
    }
}

pub(crate) fn prepare_downstream_dispatch_args(
    repo_full: &str,
    version: &str,
    envoy_version: &str,
) -> Vec<String> {
    vec![
        String::from("workflow"),
        String::from("run"),
        String::from("prepare-release.yml"),
        String::from("--repo"),
        repo_full.to_string(),
        String::from("-f"),
        format!("version={version}"),
        String::from("-f"),
        format!("envoy_version={envoy_version}"),
    ]
}

/// Compute the next downstream version and dispatch its existing
/// `prepare-release.yml` workflow with the computed inputs.
///
/// This only ever triggers the downstream repository's own unmodified
/// Prepare Release workflow, which still runs its full validation and
/// opens a draft pull request -- nothing is auto-merged. Returns the
/// computed version. With `dry_run`, prints the equivalent `gh`
/// invocation without dispatching anything.
pub fn run_release_train_prepare_downstream(
    owner: &str,
    repo: &str,
    envoy_version: &str,
    explicit_version: Option<&str>,
    bump: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    let envoy_version = SemVer::parse(envoy_version)?.to_string();
    let full = full_repo(owner, repo);
    let latest_tag = latest_release_tag(owner, repo);
    let next_version =
        resolve_next_downstream_version(bump, explicit_version, latest_tag.as_deref())?.to_string();

    let args = prepare_downstream_dispatch_args(&full, &next_version, &envoy_version);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    if dry_run {
        println!("Would run: gh {}", arg_refs.join(" "));
        return Ok(next_version);
    }

    run_gh(&arg_refs, None)?;
    println!(
        "Dispatched prepare-release.yml for {full} (version {next_version}, envoy_version \
{envoy_version})."
    );

    Ok(next_version)
}

#[cfg(test)]
mod tests {
    use super::{
        format_issue_summary, prepare_downstream_dispatch_args, resolve_next_downstream_version,
        IssueLabel, IssueSummary,
    };

    #[test]
    fn resolve_next_downstream_version_defaults_to_patch_bump() {
        let version = resolve_next_downstream_version(None, None, Some("v0.2.0"))
            .expect("patch bump should succeed");

        assert_eq!(version.to_string(), "0.2.1");
    }

    #[test]
    fn resolve_next_downstream_version_honors_explicit_bump() {
        let version = resolve_next_downstream_version(Some("minor"), None, Some("v0.2.5"))
            .expect("minor bump should succeed");

        assert_eq!(version.to_string(), "0.3.0");
    }

    #[test]
    fn resolve_next_downstream_version_prefers_explicit_version() {
        let version = resolve_next_downstream_version(None, Some("1.0.0"), Some("v0.2.0"))
            .expect("explicit version should succeed");

        assert_eq!(version.to_string(), "1.0.0");
    }

    #[test]
    fn resolve_next_downstream_version_rejects_version_and_bump_together() {
        let error = resolve_next_downstream_version(Some("patch"), Some("1.0.0"), Some("v0.2.0"))
            .expect_err("combining --version and a bump flag should fail");

        assert!(error.to_string().contains("not both"));
    }

    #[test]
    fn resolve_next_downstream_version_requires_a_latest_tag_without_explicit_version() {
        let error = resolve_next_downstream_version(None, None, None)
            .expect_err("no tag and no explicit version should fail");

        assert!(error.to_string().contains("No existing release found"));
    }

    #[test]
    fn resolve_next_downstream_version_rejects_unknown_bump_component() {
        let error = resolve_next_downstream_version(Some("giant"), None, Some("v0.2.0"))
            .expect_err("unknown bump component should fail");

        assert!(error.to_string().contains("Unknown bump component"));
    }

    #[test]
    fn prepare_downstream_dispatch_args_builds_expected_gh_invocation() {
        let args = prepare_downstream_dispatch_args("gtvfx-envoy/despatch", "0.2.0", "0.6.0");

        assert_eq!(
            args,
            vec![
                "workflow",
                "run",
                "prepare-release.yml",
                "--repo",
                "gtvfx-envoy/despatch",
                "-f",
                "version=0.2.0",
                "-f",
                "envoy_version=0.6.0",
            ]
        );
    }

    #[test]
    fn format_issue_summary_includes_release_impact_labels_only() {
        let issue = IssueSummary {
            number: 42,
            title: String::from("Review Envoy v0.6.0 compatibility and release impact"),
            labels: vec![
                IssueLabel {
                    name: String::from("dependency:envoy-core"),
                },
                IssueLabel {
                    name: String::from("release-impact:review"),
                },
            ],
        };

        assert_eq!(
            format_issue_summary(&issue),
            "#42 Review Envoy v0.6.0 compatibility and release impact [release-impact:review]"
        );
    }

    #[test]
    fn format_issue_summary_omits_brackets_without_impact_labels() {
        let issue = IssueSummary {
            number: 7,
            title: String::from("Untagged issue"),
            labels: vec![],
        };

        assert_eq!(format_issue_summary(&issue), "#7 Untagged issue");
    }
}
