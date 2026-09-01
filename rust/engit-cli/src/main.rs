//! `engit` -- Command-line interface for engit.
//!
//! Subcommands
//! -----------
//! tag             Create a semantic version git tag.
//! release         Create a GitHub release from a tag.
//! publish bundle  Publish a versioned runtime bundle.
//! publish stack   Publish a stack file to a named stack slot.
//! search          Search GitHub repositories.
//! cache validate  Validate cached bundle content against its manifest.
//! cache prune     Remove cached bundle entries by ID, pattern, or age.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand};
use engit_core::cache::{default_cache_dir, run_cache_prune, run_cache_validate, PruneSelector};
use engit_core::changelog::run_changelog;
use engit_core::cleanup::run_cleanup;
use engit_core::error::{EngitError, Result};
use engit_core::framework::run_publish_stack;
use engit_core::publish::{
    bundle_publish, default_bundle_publish_root, detect_version, is_bndlid, resolve_bndlid_to_path,
    BundlePublishOptions,
};
use engit_core::pull::run_pull;
use engit_core::release::run_release;
use engit_core::search::run_search;
use engit_core::status::run_status;
use engit_core::tag::run_tag;
use engit_core::web::run_web;

#[derive(Debug, Parser)]
#[command(
    name = "engit",
    about = "engit: git and GitHub tooling for envoy bundles.",
    version = env!("ENGIT_VERSION"),
    subcommand_required = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(
        about = "Create a semantic version git tag.",
        long_about = "Create an annotated git tag at HEAD using semantic versioning. \
The editor is pre-populated with commit bullets for changelist curation; the \
saved text becomes the tag annotation used by engit release. Provide one of \
--major / --minor / --patch to increment the current latest tag, or --version \
to supply an explicit version."
    )]
    Tag(TagArgs),

    #[command(
        about = "Create a GitHub release from a tag.",
        long_about = "Create a GitHub release using the gh CLI. Uses the selected \
tag annotation as release notes."
    )]
    Release(ReleaseArgs),

    #[command(
        about = "Show repository status.",
        long_about = "Display the current branch, ahead/behind the remote, last \
semver tag, and most recent commit."
    )]
    Status(StatusArgs),

    #[command(
        about = "Generate a changelog from GitHub releases.",
        long_about = "Fetch published GitHub releases, sort by semantic version, \
and print a formatted changelog."
    )]
    Changelog(ChangelogArgs),

    #[command(
        about = "Clean up merged and stale local branches.",
        long_about = "Prunes stale remote-tracking refs, deletes merged branches, \
and deletes branches whose remote has been deleted."
    )]
    Cleanup(CleanupArgs),

    #[command(
        about = "Open the repository on GitHub in a browser.",
        long_about = "Resolves the remote URL and opens the repository (or a \
specific branch/tag) in the default web browser."
    )]
    Web(WebArgs),

    #[command(
        about = "Pull one or more envoy bundle checkouts.",
        long_about = "Run git pull on one or more envoy bundle checkouts by bundle \
ID. Bundle paths are resolved from ENVOY_BNDL_ROOTS. Use * to pull all \
discovered bundles."
    )]
    Pull(PullArgs),

    #[command(
        about = "Search GitHub repositories.",
        long_about = "Search GitHub for repositories matching a query string. \
Default organisations are read from the ENVOY_GITHUB_ORGS environment variable \
(semicolon-separated). Use --org to override."
    )]
    Search(SearchArgs),

    #[command(
        about = "Publish bundle or stack runtime artifacts.",
        long_about = "Publish runtime-minimal bundle data or a named Envoy stack \
to its canonical studio location."
    )]
    Publish(PublishArgs),

    #[command(
        about = "Inspect or clean up the local bundle cache.",
        long_about = "Validate cached bundle content against its manifest, or \
prune cached entries by ID, glob pattern, or age."
    )]
    Cache(CacheArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("tag-version-source")
        .required(true)
        .multiple(false)
        .args(["major", "minor", "patch", "explicit_version"])
))]
struct TagArgs {
    #[arg(
        long,
        help = "Increment the major version component (resets minor and patch to 0)."
    )]
    major: bool,

    #[arg(
        long,
        help = "Increment the minor version component (resets patch to 0)."
    )]
    minor: bool,

    #[arg(long, help = "Increment the patch version component.")]
    patch: bool,

    #[arg(
        long = "version",
        short = 'v',
        value_name = "VERSION",
        help = "Explicit version string. Supports stable releases (e.g. 1.2.3, \
v1.2.3) and prerelease suffixes (e.g. 1.2.3-alpha, v0.0.1-beta). Omit the \
sequence number to auto-detect the next one."
    )]
    explicit_version: Option<String>,

    #[arg(
        long,
        short = 'm',
        value_name = "MESSAGE",
        help = "Supply the tag annotation directly, skipping the editor. Defaults \
to \"Release vMAJOR.MINOR.PATCH\"."
    )]
    message: Option<String>,

    #[arg(
        long = "print",
        short = 'p',
        help = "Print the computed next version without creating a tag."
    )]
    print_only: bool,

    #[arg(long, help = "Print the planned tag without creating it.")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    #[arg(
        long,
        value_name = "TAG",
        help = "Tag to release (e.g. v1.2.3). Defaults to the most recent \
semantic version tag."
    )]
    tag: Option<String>,

    #[arg(
        long,
        value_name = "TITLE",
        help = "Release title. Defaults to the tag string."
    )]
    title: Option<String>,

    #[arg(long, help = "Create the release as a draft.")]
    draft: bool,

    #[arg(
        long,
        default_value = "origin",
        value_name = "REMOTE",
        help = "Remote name to push to (default: origin)."
    )]
    remote: String,

    #[arg(
        long = "generate-notes",
        help = "Append GitHub auto-generated \"What's Changed\" notes from merged \
PRs to the release body."
    )]
    generate_notes: bool,

    #[arg(
        long = "print",
        short = 'p',
        help = "Print the resolved release notes without pushing or publishing."
    )]
    print_only: bool,

    #[arg(long, help = "Print the planned release without creating it.")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[arg(
        long,
        default_value = "origin",
        value_name = "REMOTE",
        help = "Remote name for ahead/behind comparison (default: origin)."
    )]
    remote: String,
}

#[derive(Debug, Args)]
struct ChangelogArgs {
    #[arg(long, value_name = "TAG", help = "Show only the release for this tag.")]
    tag: Option<String>,
}

#[derive(Debug, Args)]
struct CleanupArgs {
    #[arg(
        long,
        default_value = "origin",
        value_name = "REMOTE",
        help = "Remote name to prune (default: origin)."
    )]
    remote: String,

    #[arg(
        long,
        help = "Print what would be deleted without actually deleting anything."
    )]
    noop: bool,
}

#[derive(Debug, Args)]
struct WebArgs {
    #[arg(
        long,
        short = 'b',
        value_name = "BRANCH",
        help = "Branch or tag to view. Defaults to the current branch."
    )]
    branch: Option<String>,

    #[arg(
        long,
        default_value = "origin",
        value_name = "REMOTE",
        help = "Remote whose URL is opened (default: origin)."
    )]
    remote: String,
}

#[derive(Debug, Args)]
struct PullArgs {
    #[arg(
        value_name = "BUNDLE",
        required = true,
        num_args = 1..,
        help = "Bundle ID to pull (e.g. gt:python), or * to pull all bundles \
discovered via ENVOY_BNDL_ROOTS. Multiple IDs may be supplied."
    )]
    bundles: Vec<String>,

    #[arg(
        long,
        default_value = "origin",
        value_name = "REMOTE",
        help = "Remote to pull from (default: origin)."
    )]
    remote: String,

    #[arg(long, help = "Pass --rebase to git pull.")]
    rebase: bool,

    #[arg(long, help = "Print what would be pulled without running git.")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    #[arg(help = "Search query string.")]
    query: String,

    #[arg(
        long = "org",
        action = ArgAction::Append,
        value_name = "ORG",
        help = "GitHub organisation to search. May be specified multiple times. \
Overrides ENVOY_GITHUB_ORGS."
    )]
    orgs: Vec<String>,

    #[arg(
        long,
        default_value_t = 20,
        value_name = "N",
        help = "Maximum results per organisation (default: 20)."
    )]
    limit: usize,
}

#[derive(Debug, Args)]
struct PublishArgs {
    #[command(subcommand)]
    command: PublishCommands,
}

#[derive(Debug, Subcommand)]
enum PublishCommands {
    #[command(
        about = "Publish a versioned runtime bundle.",
        long_about = "Select runtime artifacts from a bundle and its external \
sources, then publish an immutable versioned directory. --zip adds an archive \
containing the same runtime dataset."
    )]
    Bundle(PublishBundleArgs),

    #[command(
        about = "Publish a stack file using its filename as its name.",
        long_about = "Copy a stack YAML file into a timestamped named slot \
under a stack publish root, and update the latest.estack symlink."
    )]
    Stack(PublishStackArgs),
}

#[derive(Debug, Args)]
struct PublishBundleArgs {
    #[arg(
        value_name = "PATH",
        help = "Bundle root directory or bundle ID (e.g. gt:globals). Defaults to \
the current directory."
    )]
    path: Option<String>,

    #[arg(
        long,
        short = 'o',
        value_name = "DIR",
        help = "Root directory to publish into. Defaults to \
ENVOY_BUNDLE_PUBLISH_ROOT."
    )]
    output: Option<PathBuf>,

    #[arg(
        long = "version",
        short = 'v',
        value_name = "VERSION",
        help = "Explicit version string (e.g. v1.2.0). Defaults to the latest \
semver git tag. Use \"dev\" to create a test build without requiring a git tag."
    )]
    version: Option<String>,

    #[arg(
        long = "include",
        action = ArgAction::Append,
        value_name = "GLOB",
        help = "Additional root-relative runtime glob to include. May be \
specified multiple times and applies to every publish source."
    )]
    include: Vec<String>,

    #[arg(
        long = "exclude",
        action = ArgAction::Append,
        value_name = "GLOB",
        help = "Additional root-relative glob to exclude. May be specified \
multiple times and applies to every publish source."
    )]
    exclude: Vec<String>,

    #[arg(
        long,
        help = "Also create a zip archive of the published runtime directory."
    )]
    zip: bool,

    #[arg(
        long,
        help = "Replace an existing development publish. Only valid with \
--version dev."
    )]
    force: bool,

    #[arg(
        long,
        help = "Validate and list files and destinations without writing output."
    )]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct PublishStackArgs {
    #[arg(
        value_name = "SOURCE",
        help = "Path to the stack YAML file. Its parent directory must match \
its filename stem."
    )]
    source: PathBuf,

    #[arg(
        long = "output",
        short = 'o',
        value_name = "DIR",
        help = "Root directory to publish into. Defaults to \
ENVOY_STACK_PUBLISH_ROOT."
    )]
    output: Option<PathBuf>,

    #[arg(long, help = "Show what would be written without writing anything.")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommands,
}

#[derive(Debug, Subcommand)]
enum CacheCommands {
    #[command(
        about = "Validate cached bundle content against its manifest.",
        long_about = "Recompute each cached bundle's content hash and check for \
a metadata sidecar, missing storage directories, and content-hash \
directories with no referencing cache entry."
    )]
    Validate(CacheValidateArgs),

    #[command(
        about = "Prune cached bundle entries.",
        long_about = "Remove cached entries matching --id / --pattern / \
--older-than (combined with AND semantics), and/or orphaned storage \
directories via --remove-orphans. With no selector, immediately applies \
envoy's own age/size retention policy instead of waiting for it to trigger \
on the next cache write."
    )]
    Prune(CachePruneArgs),
}

#[derive(Debug, Args)]
struct CacheValidateArgs {
    #[arg(
        long = "cache-dir",
        value_name = "DIR",
        help = "Cache directory to validate. Defaults to the resolved default \
bundle cache (ENVOY_BUNDLE_CACHE, user config, or platform default)."
    )]
    cache_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CachePruneArgs {
    #[arg(
        long = "cache-dir",
        value_name = "DIR",
        help = "Cache directory to prune. Defaults to the resolved default \
bundle cache (ENVOY_BUNDLE_CACHE, user config, or platform default)."
    )]
    cache_dir: Option<PathBuf>,

    #[arg(
        long = "id",
        action = ArgAction::Append,
        value_name = "BUNDLE_ID",
        help = "Restrict to entries with this exact bundle ID. May be repeated."
    )]
    ids: Vec<String>,

    #[arg(
        long,
        value_name = "GLOB",
        help = "Restrict to entries whose bundle ID matches this glob pattern."
    )]
    pattern: Option<String>,

    #[arg(
        long = "older-than",
        value_name = "DAYS",
        help = "Restrict to entries created more than this many days ago."
    )]
    older_than_days: Option<u64>,

    #[arg(
        long = "remove-orphans",
        help = "Also remove content-hash storage directories no longer \
referenced by the cache index."
    )]
    remove_orphans: bool,

    #[arg(
        long,
        help = "Print what would be removed without deleting anything. Not \
supported when no selector is given (the default retention policy has no \
preview mode)."
    )]
    dry_run: bool,
}

fn current_dir_path() -> Result<PathBuf> {
    env::current_dir().map_err(|source| {
        EngitError::Engit(format!("Could not determine current directory: {source}"))
    })
}

fn selected_bump(tag_args: &TagArgs) -> Option<&'static str> {
    if tag_args.major {
        Some("major")
    } else if tag_args.minor {
        Some("minor")
    } else if tag_args.patch {
        Some("patch")
    } else {
        None
    }
}

fn resolve_publish_bundle_path(spec: Option<&str>) -> Result<PathBuf> {
    match spec {
        None => current_dir_path(),
        Some(spec) if is_bndlid(spec) => {
            resolve_bndlid_to_path(spec).map_err(|error| EngitError::Engit(error.to_string()))
        }
        Some(spec) => Ok(PathBuf::from(spec)),
    }
}

fn print_published_stack(path: &Path) {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let version = path
        .parent()
        .and_then(Path::file_name)
        .map(|directory| directory.to_string_lossy().into_owned())
        .unwrap_or_default();

    println!("Published config: {name}");
    println!("  Version: {version}");
    println!("  Path:    {}", path.display());
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Tag(args) => {
            run_tag(
                selected_bump(&args),
                args.explicit_version.as_deref(),
                args.message.as_deref(),
                args.print_only,
                args.dry_run,
                None,
            )?;
        }
        Commands::Release(args) => {
            run_release(
                args.tag.as_deref(),
                args.title.as_deref(),
                args.draft,
                &args.remote,
                args.print_only,
                args.dry_run,
                args.generate_notes,
                None,
            )?;
        }
        Commands::Pull(args) => {
            run_pull(&args.bundles, &args.remote, args.rebase, args.dry_run)?;
        }
        Commands::Search(args) => {
            let orgs = if args.orgs.is_empty() {
                None
            } else {
                Some(args.orgs.as_slice())
            };
            run_search(&args.query, orgs, args.limit)?;
        }
        Commands::Status(args) => {
            run_status(&args.remote, None)?;
        }
        Commands::Changelog(args) => {
            run_changelog(args.tag.as_deref(), None)?;
        }
        Commands::Cleanup(args) => {
            run_cleanup(&args.remote, args.noop, None)?;
        }
        Commands::Web(args) => {
            run_web(args.branch.as_deref(), &args.remote, None)?;
        }
        Commands::Publish(args) => match args.command {
            PublishCommands::Bundle(args) => {
                let bundle_path = resolve_publish_bundle_path(args.path.as_deref())?;
                let version = match args.version {
                    Some(version) => version,
                    None => detect_version(&bundle_path)?,
                };
                let output_dir = match args.output {
                    Some(path) => path,
                    None => default_bundle_publish_root()?,
                };
                let options = BundlePublishOptions {
                    output_dir: &output_dir,
                    version: &version,
                    create_zip: args.zip,
                    extra_includes: &args.include,
                    extra_excludes: &args.exclude,
                    force: args.force,
                    dry_run: args.dry_run,
                };
                let result = bundle_publish(&bundle_path, &options)?;

                if !args.dry_run {
                    println!("Published folder: {}", result.directory.display());
                    if let Some(archive) = result.archive {
                        println!("Published zip: {}", archive.display());
                    }
                }
            }
            PublishCommands::Stack(args) => {
                let result = run_publish_stack(args.output.as_deref(), &args.source, args.dry_run)?;

                if !args.dry_run {
                    print_published_stack(&result);
                }
            }
        },
        Commands::Cache(args) => match args.command {
            CacheCommands::Validate(args) => {
                let cache_dir = resolve_cache_dir(args.cache_dir)?;
                let report = run_cache_validate(&cache_dir)?;
                if report.has_problems() {
                    return Err(EngitError::Cache(
                        "Cache validation found problems (see above).".to_string(),
                    ));
                }
            }
            CacheCommands::Prune(args) => {
                let cache_dir = resolve_cache_dir(args.cache_dir)?;
                let older_than = args
                    .older_than_days
                    .map(|days| {
                        days.checked_mul(86_400)
                            .map(Duration::from_secs)
                            .ok_or_else(|| {
                                EngitError::Cache(
                                    "The value passed to --older-than-days is too large."
                                        .to_string(),
                                )
                            })
                    })
                    .transpose()?;
                let selector = PruneSelector {
                    ids: args.ids,
                    pattern: args.pattern,
                    older_than,
                    remove_orphans: args.remove_orphans,
                };
                run_cache_prune(&cache_dir, &selector, args.dry_run)?;
            }
        },
    }

    Ok(())
}

fn resolve_cache_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit.or_else(default_cache_dir).ok_or_else(|| {
        EngitError::Cache(
            "No cache directory to operate on: ENVOY_DISABLE_BUNDLE_CACHE is set and no \
             --cache-dir was given."
                .to_string(),
        )
    })
}

fn main() -> ExitCode {
    // Python's CLI explicitly translated KeyboardInterrupt into exit code 130.
    // This native CLI currently relies on Rust's default Ctrl+C termination
    // behavior instead, so that parity detail remains a small known gap.
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::from(1)
        }
    }
}
