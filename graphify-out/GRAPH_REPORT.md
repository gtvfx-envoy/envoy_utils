# Graph Report - .  (2026-07-31)

## Corpus Check
- cluster-only mode — file stats not available

## Summary
- 433 nodes · 1039 edges · 18 communities (17 shown, 1 thin omitted)
- Extraction: 94% EXTRACTED · 6% INFERRED · 0% AMBIGUOUS · INFERRED: 58 edges (avg confidence: 0.79)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- publish.rs
- git.rs
- release_automation.py
- github.rs
- tag.rs
- SemVer
- main.rs
- error.rs
- framework.rs
- resolve_specs
- package_release.py
- cli.rs
- ReleaseAutomationTests
- run_web
- EnvVarGuard
- git_describe
- parseArguments
- engit

## God Nodes (most connected - your core abstractions)
1. `run_git()` - 28 edges
2. `SemVer` - 24 edges
3. `run()` - 18 edges
4. `run_release()` - 18 edges
5. `bundle_publish()` - 17 edges
6. `run_tag()` - 15 edges
7. `create_bundle()` - 14 edges
8. `list_publish_files()` - 13 edges
9. `write_file()` - 13 edges
10. `require_git_repo()` - 12 edges

## Surprising Connections (you probably didn't know these)
- `resolve_publish_bundle_path()` --calls--> `is_bndlid()`  [INFERRED]
  rust/engit-cli/src/main.rs → rust/engit-core/src/publish.rs
- `resolve_publish_bundle_path()` --calls--> `resolve_bndlid_to_path()`  [INFERRED]
  rust/engit-cli/src/main.rs → rust/engit-core/src/publish.rs
- `run()` --calls--> `run_changelog()`  [INFERRED]
  rust/engit-cli/src/main.rs → rust/engit-core/src/changelog.rs
- `run()` --calls--> `run_cleanup()`  [INFERRED]
  rust/engit-cli/src/main.rs → rust/engit-core/src/cleanup.rs
- `run()` --calls--> `run_publish_stack()`  [INFERRED]
  rust/engit-cli/src/main.rs → rust/engit-core/src/framework.rs

## Import Cycles
- None detected.

## Communities (18 total, 1 thin omitted)

### Community 0 - "publish.rs"
Cohesion: 0.10
Nodes (63): Glob, GlobSet, HashSet, PublishFiles, ArtifactManifest, base_version(), bndlid_from_name(), build_glob_set() (+55 more)

### Community 1 - "git.rs"
Cohesion: 0.10
Nodes (57): I, Option, Path, Result, run_cleanup(), args_to_strings(), create_tag(), delete_local_branch() (+49 more)

### Community 2 - "release_automation.py"
Cohesion: 0.10
Nodes (43): ArgumentParser, Pattern, buildIssueReport(), buildParser(), checkRelease(), classifyImpact(), gitOutput(), lockfileHasDependencyChanges() (+35 more)

### Community 3 - "github.rs"
Cohesion: 0.11
Nodes (35): fetch_release_detail(), ReleaseDetail, Option, Path, Result, String, Vec, run_changelog() (+27 more)

### Community 4 - "tag.rs"
Cohesion: 0.11
Nodes (31): EnvVarGuard, find_editor(), open_in_editor(), remove_if_empty(), Drop, Option, Path, PathBuf (+23 more)

### Community 5 - "SemVer"
Cohesion: 0.12
Nodes (20): Display, Formatter, FromStr, Ord, Ordering, PartialOrd, Regex, bump_helpers_reset_lower_parts() (+12 more)

### Community 6 - "main.rs"
Cohesion: 0.18
Nodes (28): ExitCode, ChangelogArgs, CleanupArgs, Cli, Commands, current_dir_path(), main(), print_published_stack() (+20 more)

### Community 7 - "error.rs"
Cohesion: 0.28
Nodes (10): Into, command_failure_falls_back_to_stdout(), command_failure_prefers_stderr(), command_failure_uses_generic_message_when_empty(), EngitError, format_command_failure(), Error, PathBuf (+2 more)

### Community 8 - "framework.rs"
Cohesion: 0.17
Nodes (12): default_stack_root_from_env(), EnvVarGuard, publishes_stack_to_explicit_root(), Drop, Option, OsStr, OsString, Path (+4 more)

### Community 9 - "resolve_specs"
Cohesion: 0.17
Nodes (13): EnvVarGuard, resolve_specs(), resolves_explicit_and_wildcard_bundle_specs(), Drop, Option, OsStr, OsString, PathBuf (+5 more)

### Community 10 - "package_release.py"
Cohesion: 0.19
Nodes (14): copyReleaseFiles(), main(), normalizeTarMetadata(), parseArguments(), Namespace, Path, Create one platform-specific Envoy Utils release archive. This is a release-…, Make archive ownership and permissions host-independent. Args: member: Archive… (+6 more)

### Community 11 - "cli.rs"
Cohesion: 0.33
Nodes (10): Assert, help_lists_all_subcommands(), missing_required_argument_returns_usage_error(), publish_help_lists_bundle_and_stack_subcommands(), publish_stack_without_stack_root_or_env_var_fails_with_expected_message(), retired_publish_stack_command_is_rejected(), String, stderr_text() (+2 more)

### Community 12 - "ReleaseAutomationTests"
Cohesion: 0.20
Nodes (6): Tests for Envoy Utils release automation., Exercise deterministic release metadata rewrites., SemVer validation accepts stable and prerelease values., The Cargo tag and exact version move together., Candidate testing removes the remote exact-version constraint., ReleaseAutomationTests

### Community 13 - "run_web"
Cohesion: 0.33
Nodes (7): open_url(), Option, Path, Result, String, run_web(), to_https_url()

### Community 14 - "EnvVarGuard"
Cohesion: 0.29
Nodes (6): EnvVarGuard, Drop, Option, OsStr, OsString, Self

### Community 15 - "git_describe"
Cohesion: 0.43
Nodes (6): git_describe(), main(), repo_root(), Option, PathBuf, String

### Community 16 - "parseArguments"
Cohesion: 0.33
Nodes (6): main(), parseArguments(), Namespace, Build the native Envoy Utils command-line tools., Parse build arguments. Returns: Parsed command-line options., Build the Rust workspace. Returns: Process exit status.

## Knowledge Gaps
- **1 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `run()` connect `main.rs` to `publish.rs`, `git.rs`, `github.rs`, `tag.rs`, `framework.rs`, `resolve_specs`, `run_web`?**
  _High betweenness centrality (0.103) - this node is a cross-community bridge._
- **Why does `SemVer` connect `SemVer` to `git.rs`, `tag.rs`?**
  _High betweenness centrality (0.070) - this node is a cross-community bridge._
- **Why does `run_tag()` connect `tag.rs` to `git.rs`, `SemVer`, `main.rs`?**
  _High betweenness centrality (0.039) - this node is a cross-community bridge._
- **Should `publish.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.09745390693590869 - nodes in this community are weakly interconnected._
- **Should `git.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.10291858678955453 - nodes in this community are weakly interconnected._
- **Should `release_automation.py` be split into smaller, more focused modules?**
  _Cohesion score 0.09830866807610994 - nodes in this community are weakly interconnected._
- **Should `github.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.11153846153846154 - nodes in this community are weakly interconnected._