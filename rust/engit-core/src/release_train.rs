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

fn latest_release_tag(owner: &str, repo: &str) -> Result<Option<String>> {
    let full = full_repo(owner, repo);
    let release_view_result = run_gh(
        &[
            "release",
            "view",
            "--repo",
            full.as_str(),
            "--json",
            "tagName",
        ],
        None,
    );
    classify_latest_release_lookup(release_view_result, || {
        // `gh release view` reports the identical "release not found"
        // message whether a repository genuinely has no releases yet, does
        // not exist, or is inaccessible -- that text alone cannot
        // distinguish them (verified against the real `gh` CLI). Confirm
        // the repository itself is reachable before treating a failure as
        // a genuine "no releases yet" rather than a missing repository or
        // an auth/network problem.
        run_gh(&["repo", "view", full.as_str(), "--json", "name"], None).is_ok()
    })
}

/// Decide the outcome of a release lookup from its raw result, only
/// consulting `repo_is_reachable` when the lookup itself failed. Split out
/// from [`latest_release_tag`] so this decision is unit-testable without
/// shelling out to `gh`.
fn classify_latest_release_lookup(
    release_view_result: Result<String>,
    repo_is_reachable: impl FnOnce() -> bool,
) -> Result<Option<String>> {
    match release_view_result {
        Ok(output) => {
            let parsed: LatestReleaseTag = serde_json::from_str(&output).map_err(|source| {
                EngitError::ReleaseTrain(format!(
                    "Could not parse `gh release view` output: {source}"
                ))
            })?;
            Ok(Some(parsed.tag_name))
        }
        Err(release_error) => {
            if repo_is_reachable() {
                Ok(None)
            } else {
                Err(release_error)
            }
        }
    }
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
            Ok(Some(tag)) => println!("  Latest release: {tag}"),
            Ok(None) => println!("  Latest release: (none found)"),
            Err(error) => println!("  Latest release: (could not query: {error})"),
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
    let latest_tag = latest_release_tag(owner, repo)?;
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
        classify_latest_release_lookup, format_issue_summary, prepare_downstream_dispatch_args,
        resolve_next_downstream_version, IssueLabel, IssueSummary,
    };
    use crate::error::EngitError;

    #[test]
    fn classify_latest_release_lookup_returns_tag_on_success() {
        let result =
            classify_latest_release_lookup(Ok(String::from(r#"{"tagName":"v1.2.3"}"#)), || {
                panic!("reachability must not be checked when the lookup succeeded")
            })
            .expect("valid JSON should classify successfully");

        assert_eq!(result, Some(String::from("v1.2.3")));
    }

    #[test]
    fn classify_latest_release_lookup_surfaces_malformed_json_without_reachability_check() {
        let error = classify_latest_release_lookup(Ok(String::from("not json")), || {
            panic!("reachability must not be checked for a parse failure")
        })
        .expect_err("malformed JSON should be a real error, not None");

        assert!(matches!(error, EngitError::ReleaseTrain(_)));
    }

    #[test]
    fn classify_latest_release_lookup_treats_failure_as_no_releases_when_repo_is_reachable() {
        let result = classify_latest_release_lookup(
            Err(EngitError::GitHub(String::from("release not found"))),
            || true,
        )
        .expect("a reachable repository with no releases should classify as None");

        assert_eq!(result, None);
    }

    #[test]
    fn classify_latest_release_lookup_propagates_error_when_repo_is_unreachable() {
        // Verified empirically: `gh release view` reports the identical
        // "release not found" message for a repository with no releases
        // and for one that does not exist at all, so this exact scenario
        // (repo unreachable) must still surface as an error rather than
        // silently becoming None.
        let error = classify_latest_release_lookup(
            Err(EngitError::GitHub(String::from("release not found"))),
            || false,
        )
        .expect_err("an unreachable repository must propagate the original error");

        assert!(error.to_string().contains("release not found"));
    }

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
