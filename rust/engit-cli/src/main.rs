//! `engit` -- Command-line interface for engit.
//!
//! Subcommands
//! -----------
//! tag             Create a semantic version git tag.
//! release         Create a GitHub release from a tag.
//! publish         Create a versioned publish of a bundle (folder or zip).
//! publish-config  Publish a bundles config file to a named config slot.
//! search          Search GitHub repositories.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand};
use engit_core::changelog::run_changelog;
use engit_core::cleanup::run_cleanup;
use engit_core::error::{EngitError, Result};
use engit_core::publish::{bundle_publish, detect_version, is_bndlid, resolve_bndlid_to_path};
use engit_core::pull::run_pull;
use engit_core::release::run_release;
use engit_core::search::run_search;
use engit_core::status::run_status;
use engit_core::tag::run_tag;
use engit_core::web::run_web;
use envoy_core::config_registry::{publish_config, CFG_ROOTS_VAR};

#[derive(Debug, Parser)]
#[command(
    name = "engit",
    about = "engit: git and GitHub tooling for envoy bundles.",
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
        about = "Create a versioned publish of a bundle.",
        long_about = "Copy a bundle into a clean versioned output directory or zip \
archive, stripping git and build artefacts. Output layout: \
<output>/<bundle-name>/<version>/ or, with --zip: \
<output>/<bundle-name>-<version>.zip"
    )]
    Publish(PublishArgs),

    #[command(
        name = "publish-config",
        about = "Publish a bundles config file to a named config slot.",
        long_about = "Copy a bundles-config JSON file into a versioned named slot \
under a config root directory, and update the \"latest\" pointer."
    )]
    PublishConfig(PublishConfigArgs),
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
        help = "Root directory to write the output into. Defaults to the current \
directory."
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
        long = "exclude",
        action = ArgAction::Append,
        value_name = "PATTERN",
        help = "Additional glob pattern to exclude (e.g. \"*.spec\"). May be \
specified multiple times."
    )]
    exclude: Vec<String>,

    #[arg(long, help = "Create a zip archive instead of a versioned directory.")]
    zip: bool,

    #[arg(
        long,
        help = "List the files that would be included without writing any output."
    )]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct PublishConfigArgs {
    #[arg(
        value_name = "NAME",
        help = "Named config slot (e.g. studio, dev, production)."
    )]
    name: String,

    #[arg(
        value_name = "SOURCE",
        help = "Path to the bundles-config JSON file to publish."
    )]
    source: PathBuf,

    #[arg(
        long = "cfg-root",
        short = 'r',
        value_name = "DIR",
        help = "Root directory to publish into. Defaults to the first directory \
in ENVOY_CFG_ROOTS."
    )]
    cfg_root: Option<PathBuf>,

    #[arg(long, help = "Show what would be written without writing anything.")]
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

fn default_cfg_root_from_env() -> Result<PathBuf> {
    // `envoy_core::config_registry::cfg_roots()` is intentionally private, so
    // the CLI mirrors only the tiny bit of Python parity logic it needs here:
    // read ENVOY_CFG_ROOTS, split by the platform separator, trim entries, and
    // use the first non-empty root when --cfg-root is omitted.
    let separator = if cfg!(windows) { ';' } else { ':' };

    env::var(CFG_ROOTS_VAR)
        .ok()
        .and_then(|raw| {
            raw.split(separator)
                .map(str::trim)
                .find(|entry| !entry.is_empty())
                .map(PathBuf::from)
        })
        .ok_or_else(|| {
            EngitError::Engit(format!(
                "No --cfg-root specified and {CFG_ROOTS_VAR} is not set."
            ))
        })
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

fn print_published_config(name: &str, path: &Path) {
    let version = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
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
        Commands::Publish(args) => {
            let bundle_path = resolve_publish_bundle_path(args.path.as_deref())?;
            let version = match args.version {
                Some(version) => version,
                None => detect_version(&bundle_path)?,
            };
            let output_dir = match args.output {
                Some(path) => path,
                None => current_dir_path()?,
            };
            let result = bundle_publish(
                &bundle_path,
                &output_dir,
                &version,
                args.zip,
                Some(args.exclude.as_slice()),
                args.dry_run,
            )?;

            if !args.dry_run {
                let label = if args.zip { "zip" } else { "folder" };
                println!("Published {label}: {}", result.display());
            }
        }
        Commands::PublishConfig(args) => {
            let cfg_root = match args.cfg_root {
                Some(path) => path,
                None => default_cfg_root_from_env()?,
            };
            let result = publish_config(&cfg_root, &args.name, &args.source, args.dry_run)?;

            if !args.dry_run {
                print_published_config(&args.name, &result);
            }
        }
    }

    Ok(())
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
