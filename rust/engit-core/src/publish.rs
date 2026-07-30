//! Bundle publish helpers.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::{EngitError, Result};
use crate::git::{get_latest_semver_tag, get_remote_url, is_git_repo};

/// Version string used for local test builds.
pub const DEV_VERSION: &str = "dev";
/// Marker file written at the root of every published bundle.
pub const BUNDLE_MARKER_FILE: &str = ".bundle";
/// Publish manifest filename under `.envoy/`.
pub const PUBLISH_MANIFEST_FILE: &str = "publish-manifest.yaml";
/// Retired artifact-source config filename under `.envoy/`.
pub const LEGACY_BUNDLE_ARTIFACTS_FILE: &str = "bundle-artifacts.json";
/// Per-bundle envoy configuration directory.
pub const BUNDLE_ENV_DIR: &str = ".envoy";
/// Preferred canonical bundle publish root environment variable.
pub const BUNDLE_PUBLISH_ROOT_VAR: &str = "ENVOY_BUNDLE_PUBLISH_ROOT";
/// Deprecated canonical bundle publish root environment variable.
pub const LEGACY_BUNDLE_PUBLISH_ROOT_VAR: &str = "ENVOY_BNDLE_PROD";
/// Runtime paths included from every declared source root.
pub const DEFAULT_INCLUDES: &[&str] = &[
    ".envoy/**",
    "py/**",
    "bin/**",
    "prebuilt/**",
    "resources/**",
    "resource/**",
    "docs/**",
    "LICENSE*",
    "NOTICE*",
    "THIRD_PARTY_LICENSES*",
];
/// Build, VCS, and cache paths excluded from every declared source root.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".git/**",
    ".gitignore",
    "**/.gitignore",
    ".gitattributes",
    "**/.gitattributes",
    ".gitmodules",
    "**/.gitmodules",
    ".github",
    ".github/**",
    ".hg",
    ".hg/**",
    ".svn",
    ".svn/**",
    "**/build",
    "**/build/**",
    "**/dist",
    "**/dist/**",
    "**/target",
    "**/target/**",
    "**/.pytest_cache",
    "**/.pytest_cache/**",
    "**/.ruff_cache",
    "**/.ruff_cache/**",
    "**/.mypy_cache",
    "**/.mypy_cache/**",
    "**/__pycache__",
    "**/__pycache__/**",
    "**/*.pyc",
    "**/*.pyo",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct BundleArtifact {
    source: PathBuf,
    destination: PathBuf,
    include: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct PublishManifest {
    include: Vec<String>,
    exclude: Vec<String>,
    artifacts: Vec<ArtifactManifest>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactManifest {
    source: String,
    #[serde(default = "default_destination")]
    destination: PathBuf,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Serialize)]
struct BundleMarkerData<'a> {
    bndlid: &'a str,
    name: &'a str,
    version: &'a str,
    published: String,
}

/// Paths created by a bundle publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundlePublishResult {
    /// Canonical runtime directory.
    pub directory: PathBuf,
    /// Optional zip archive generated alongside the runtime directory.
    pub archive: Option<PathBuf>,
}

/// Settings for a bundle publish operation.
#[derive(Clone, Copy, Debug)]
pub struct BundlePublishOptions<'a> {
    /// Root directory that receives the versioned publish.
    pub output_dir: &'a Path,
    /// Version directory name, or [`DEV_VERSION`] for a development publish.
    pub version: &'a str,
    /// Whether to create a zip archive alongside the runtime directory.
    pub create_zip: bool,
    /// Additional runtime globs applied to every publish source.
    pub extra_includes: &'a [String],
    /// Additional exclusion globs applied to every publish source.
    pub extra_excludes: &'a [String],
    /// Whether an existing development publish may be replaced.
    pub force: bool,
    /// Whether to validate and report the publish without writing it.
    pub dry_run: bool,
}

type PublishFiles = Vec<(PathBuf, PathBuf)>;

struct SourceSelection {
    files: PublishFiles,
    matched_cli: HashSet<String>,
}

fn default_destination() -> PathBuf {
    PathBuf::from(".")
}

/// Return the latest semver tag for the bundle's git repository.
pub fn detect_version(bundle_path: &Path) -> Result<String> {
    if !is_git_repo(Some(bundle_path)) {
        return Err(EngitError::Publish(format!(
            "'{}' is not inside a git repository. Use --version dev for a \
test build without a git tag.",
            bundle_path.display()
        )));
    }

    let tag = get_latest_semver_tag(Some(bundle_path)).ok_or_else(|| {
        EngitError::Publish(format!(
            "No semantic version tags found in '{}'. Create one with 'engit \
tag' or use --version dev.",
            bundle_path.display()
        ))
    })?;

    Ok(tag.to_string())
}

/// Convert a bundle directory name to a bundle ID.
pub fn bndlid_from_name(bundle_name: &str) -> String {
    bundle_name.replace('-', ":")
}

/// Convert a bundle name to its publish directory path.
pub fn publish_path(bundle_name: &str) -> PathBuf {
    PathBuf::from_iter(bundle_name.split('-'))
}

/// Return `true` if `spec` looks like a bundle ID rather than a path.
pub fn is_bndlid(spec: &str) -> bool {
    if spec.is_empty() || matches!(spec.chars().next(), Some('/' | '\\' | '.' | '~')) {
        return false;
    }
    matches!(spec.find(':'), Some(index) if index >= 2)
}

/// Resolve a bundle ID to a filesystem path via `ENVOY_BNDL_ROOTS`.
pub fn resolve_bndlid_to_path(bndlid: &str) -> Result<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let roots_str = env::var("ENVOY_BNDL_ROOTS").unwrap_or_default();
    if roots_str.is_empty() {
        return Err(EngitError::Publish(format!(
            "Cannot resolve bundle ID {bndlid:?}: ENVOY_BNDL_ROOTS is not set."
        )));
    }

    let roots: Vec<PathBuf> = roots_str
        .split(separator)
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .collect();
    let segments: Vec<&str> = bndlid.split(':').collect();

    for root in &roots {
        let candidate = root.join(PathBuf::from_iter(segments.iter().copied()));
        if candidate.is_dir() {
            return Ok(candidate.canonicalize().unwrap_or(candidate));
        }
    }

    let searched = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(EngitError::Publish(format!(
        "Bundle {bndlid:?} not found in ENVOY_BNDL_ROOTS ({searched})."
    )))
}

/// Derive the bundle (repo) name for a bundle directory.
pub fn repo_name_from(bundle_path: &Path) -> String {
    if let Some(url) = get_remote_url("origin", Some(bundle_path)) {
        let mut name = url.trim_end_matches('/').to_string();
        if name.ends_with(".git") {
            name.truncate(name.len() - 4);
        }
        if let Some(last) = name
            .rsplit('/')
            .next()
            .and_then(|value| value.rsplit(':').next())
        {
            if !last.is_empty() {
                return last.to_string();
            }
        }
    }

    bundle_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn bundle_marker_data<'a>(
    bndlid: &'a str,
    bndl_name: &'a str,
    version: &'a str,
) -> BundleMarkerData<'a> {
    BundleMarkerData {
        bndlid,
        name: bndl_name,
        version,
        published: Utc::now().to_rfc3339(),
    }
}

/// Strip the `-envoy.<int>` iteration suffix from a version string.
pub fn base_version(version: &str) -> String {
    let regex = Regex::new(r"-envoy\.\d+$").expect("base-version regex must compile");
    regex.replace(version, "").into_owned()
}

/// Expand `${VAR}` tokens in `value` against built-ins and environment vars.
pub fn resolve_asset_tokens(value: &str, version: &str) -> String {
    let regex = Regex::new(r"\$\{([^}]+)\}").expect("asset-token regex must compile");
    let base_version = base_version(version);

    regex
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let name = captures
                .get(1)
                .expect("token regex must capture one group")
                .as_str();
            match name {
                "VERSION" => version.to_string(),
                "BASE_VERSION" => base_version.clone(),
                _ => env::var(name).unwrap_or_else(|_| captures[0].to_string()),
            }
        })
        .into_owned()
}

fn load_publish_manifest(
    bundle_path: &Path,
    version: &str,
) -> Result<(PublishManifest, Vec<BundleArtifact>)> {
    let envoy_dir = bundle_path.join(BUNDLE_ENV_DIR);
    let legacy_path = envoy_dir.join(LEGACY_BUNDLE_ARTIFACTS_FILE);
    if legacy_path.is_file() {
        return Err(EngitError::Publish(format!(
            "Legacy publish manifest '{}' is no longer supported. Migrate it to '{}'.",
            legacy_path.display(),
            envoy_dir.join(PUBLISH_MANIFEST_FILE).display()
        )));
    }

    let manifest_path = envoy_dir.join(PUBLISH_MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Ok((PublishManifest::default(), Vec::new()));
    }

    let contents = fs::read_to_string(&manifest_path)
        .map_err(|source| EngitError::io(manifest_path.clone(), source))?;
    let manifest = serde_yaml::from_str::<PublishManifest>(&contents)
        .map_err(|source| EngitError::yaml(manifest_path.clone(), source))?;

    validate_patterns(&manifest.include, "manifest include")?;
    validate_patterns(&manifest.exclude, "manifest exclude")?;

    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        validate_destination(&artifact.destination)?;
        validate_patterns(&artifact.include, "artifact include")?;
        validate_patterns(&artifact.exclude, "artifact exclude")?;

        let expanded_source = resolve_asset_tokens_strict(&artifact.source, version)?;
        let source = PathBuf::from(expanded_source);
        let source = if source.is_absolute() {
            source
        } else {
            bundle_path.join(source)
        };
        if !source.is_dir() {
            return Err(EngitError::Publish(format!(
                "Publish artifact source does not exist or is not a directory: '{}'",
                source.display()
            )));
        }

        artifacts.push(BundleArtifact {
            source: source.canonicalize().unwrap_or(source),
            destination: artifact.destination.clone(),
            include: artifact.include.clone(),
            exclude: artifact.exclude.clone(),
        });
    }

    Ok((manifest, artifacts))
}

/// Resolve the canonical bundle publish root from the environment.
pub fn default_bundle_publish_root() -> Result<PathBuf> {
    if let Some(root) = env::var_os(BUNDLE_PUBLISH_ROOT_VAR).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    if let Some(root) =
        env::var_os(LEGACY_BUNDLE_PUBLISH_ROOT_VAR).filter(|value| !value.is_empty())
    {
        eprintln!(
            "warning: {LEGACY_BUNDLE_PUBLISH_ROOT_VAR} is deprecated; use \
{BUNDLE_PUBLISH_ROOT_VAR} instead."
        );
        return Ok(PathBuf::from(root));
    }
    Err(EngitError::Publish(format!(
        "No --output specified and neither {BUNDLE_PUBLISH_ROOT_VAR} nor \
{LEGACY_BUNDLE_PUBLISH_ROOT_VAR} is set."
    )))
}

fn resolve_asset_tokens_strict(value: &str, version: &str) -> Result<String> {
    let regex = Regex::new(r"\$\{([^}]+)\}").expect("asset-token regex must compile");
    let base_version = base_version(version);
    let mut missing = Vec::new();

    let expanded = regex
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let name = captures
                .get(1)
                .expect("token regex must capture one group")
                .as_str();
            match name {
                "VERSION" => version.to_string(),
                "BASE_VERSION" => base_version.clone(),
                _ => match env::var(name) {
                    Ok(value) => value,
                    Err(_) => {
                        missing.push(name.to_string());
                        captures[0].to_string()
                    }
                },
            }
        })
        .into_owned();

    if missing.is_empty() {
        Ok(expanded)
    } else {
        missing.sort();
        missing.dedup();
        Err(EngitError::Publish(format!(
            "Unresolved environment variable(s) in publish manifest: {}",
            missing.join(", ")
        )))
    }
}

fn validate_destination(destination: &Path) -> Result<()> {
    if destination.as_os_str().is_empty()
        || destination
            .components()
            .any(|component| !matches!(component, Component::CurDir | Component::Normal(_)))
    {
        return Err(EngitError::Publish(format!(
            "Artifact destination must be a relative path without '..': '{}'",
            destination.display()
        )));
    }
    Ok(())
}

fn validate_patterns(patterns: &[String], label: &str) -> Result<()> {
    for pattern in patterns {
        compile_glob(pattern).map_err(|error| {
            EngitError::Publish(format!("Invalid {label} pattern {pattern:?}: {error}"))
        })?;
    }
    Ok(())
}

fn compile_glob(pattern: &str) -> std::result::Result<Glob, globset::Error> {
    Glob::new(&pattern.replace('\\', "/"))
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(compile_glob(pattern).map_err(|error| {
            EngitError::Publish(format!("Invalid publish pattern {pattern:?}: {error}"))
        })?);
    }
    builder
        .build()
        .map_err(|error| EngitError::Publish(format!("Could not build publish patterns: {error}")))
}

fn normalize_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn walk_source(
    current: &Path,
    base: &Path,
    canonical_base: &Path,
    excludes: &GlobSet,
    ancestors: &mut HashSet<PathBuf>,
    collected: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical_current = current
        .canonicalize()
        .map_err(|source| EngitError::io(current.to_path_buf(), source))?;
    if !canonical_current.starts_with(canonical_base) {
        return Ok(());
    }
    if !ancestors.insert(canonical_current.clone()) {
        return Ok(());
    }

    let read_dir =
        fs::read_dir(current).map_err(|source| EngitError::io(current.to_path_buf(), source))?;
    let mut entries = read_dir
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| EngitError::io(current.to_path_buf(), source))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(base).map_err(|_| {
            EngitError::Publish(format!(
                "Could not make '{}' relative to source '{}'.",
                path.display(),
                base.display()
            ))
        })?;
        let normalized = normalize_relative(relative);
        if excludes.is_match(&normalized) {
            continue;
        }

        let metadata =
            fs::symlink_metadata(&path).map_err(|source| EngitError::io(path.clone(), source))?;
        if metadata.file_type().is_symlink() {
            let target = path
                .canonicalize()
                .map_err(|source| EngitError::io(path.clone(), source))?;
            if !target.starts_with(canonical_base) {
                continue;
            }
        }

        if path.is_dir() {
            walk_source(&path, base, canonical_base, excludes, ancestors, collected)?;
        } else if path.is_file() {
            collected.push(relative.to_path_buf());
        }
    }

    ancestors.remove(&canonical_current);
    Ok(())
}

fn expanded_include_pattern(source: &Path, pattern: &str) -> String {
    let normalized = pattern.replace('\\', "/");
    if !normalized.contains(['*', '?', '[', ']']) && source.join(pattern).is_dir() {
        format!("{}/**", normalized.trim_end_matches('/'))
    } else {
        normalized
    }
}

fn collect_source_files(
    source: &Path,
    includes: &[String],
    excludes: &[String],
    cli_includes: &[String],
    cli_excludes: &[String],
) -> Result<SourceSelection> {
    let canonical_source = source
        .canonicalize()
        .map_err(|error| EngitError::io(source.to_path_buf(), error))?;

    let expanded_includes = includes
        .iter()
        .map(|pattern| expanded_include_pattern(source, pattern))
        .collect::<Vec<_>>();
    let expanded_cli_includes = cli_includes
        .iter()
        .map(|pattern| expanded_include_pattern(source, pattern))
        .collect::<Vec<_>>();

    let mut all_includes = DEFAULT_INCLUDES
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect::<Vec<_>>();
    all_includes.extend(expanded_includes.iter().cloned());
    all_includes.extend(expanded_cli_includes.iter().cloned());

    let mut all_excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect::<Vec<_>>();
    all_excludes.extend(excludes.iter().map(|pattern| pattern.replace('\\', "/")));
    all_excludes.extend(
        cli_excludes
            .iter()
            .map(|pattern| pattern.replace('\\', "/")),
    );

    let include_set = build_glob_set(&all_includes)?;
    let exclude_set = build_glob_set(&all_excludes)?;
    let mut relative_files = Vec::new();
    walk_source(
        source,
        source,
        &canonical_source,
        &exclude_set,
        &mut HashSet::new(),
        &mut relative_files,
    )?;
    relative_files.sort();

    let normalized_files = relative_files
        .iter()
        .map(|relative| normalize_relative(relative))
        .collect::<Vec<_>>();
    for (original, expanded) in includes.iter().zip(&expanded_includes) {
        let matcher = compile_glob(expanded)
            .map_err(|error| EngitError::Publish(error.to_string()))?
            .compile_matcher();
        if !normalized_files.iter().any(|path| matcher.is_match(path)) {
            return Err(EngitError::Publish(format!(
                "Explicit include pattern {original:?} matched no files under '{}'.",
                source.display()
            )));
        }
    }

    let mut matched_cli = HashSet::new();
    for (original, expanded) in cli_includes.iter().zip(&expanded_cli_includes) {
        let matcher = compile_glob(expanded)
            .map_err(|error| EngitError::Publish(error.to_string()))?
            .compile_matcher();
        if normalized_files.iter().any(|path| matcher.is_match(path)) {
            matched_cli.insert(original.clone());
        }
    }

    let files = relative_files
        .into_iter()
        .filter(|relative| {
            let normalized = normalize_relative(relative);
            include_set.is_match(&normalized) && !exclude_set.is_match(&normalized)
        })
        .map(|relative| (source.join(&relative), relative))
        .collect();
    Ok(SourceSelection { files, matched_cli })
}

/// Return the list of files that would be included in a publish.
pub fn list_publish_files(
    bundle_path: &Path,
    version: &str,
    extra_includes: &[String],
    extra_excludes: &[String],
) -> Result<PublishFiles> {
    let bundle_path = bundle_path
        .canonicalize()
        .unwrap_or_else(|_| bundle_path.to_path_buf());

    validate_patterns(extra_includes, "CLI include")?;
    validate_patterns(extra_excludes, "CLI exclude")?;
    let (manifest, artifacts) = load_publish_manifest(&bundle_path, version)?;
    let bundle_selection = collect_source_files(
        &bundle_path,
        &manifest.include,
        &manifest.exclude,
        extra_includes,
        extra_excludes,
    )?;
    let mut matched_cli = bundle_selection.matched_cli;

    let mut destinations = BTreeMap::<PathBuf, PathBuf>::new();
    for (source, relative) in bundle_selection.files {
        destinations.insert(relative, source);
    }
    for artifact in artifacts {
        let artifact_selection = collect_source_files(
            &artifact.source,
            &artifact.include,
            &artifact.exclude,
            extra_includes,
            extra_excludes,
        )?;
        matched_cli.extend(artifact_selection.matched_cli);
        for (source, relative) in artifact_selection.files {
            let destination = if artifact.destination == Path::new(".") {
                relative
            } else {
                artifact.destination.join(relative)
            };
            if let Some(existing) = destinations.get(&destination) {
                return Err(EngitError::Publish(format!(
                    "Publish destination collision at '{}': '{}' and '{}'.",
                    destination.display(),
                    existing.display(),
                    source.display()
                )));
            }
            destinations.insert(destination, source);
        }
    }

    for pattern in extra_includes {
        if !matched_cli.contains(pattern) {
            return Err(EngitError::Publish(format!(
                "CLI include pattern {pattern:?} matched no files in any publish source."
            )));
        }
    }

    Ok(destinations
        .into_iter()
        .map(|(destination, source)| (source, destination))
        .collect())
}

/// Create a versioned publish of `bundle_path`.
pub fn bundle_publish(
    bundle_path: &Path,
    options: &BundlePublishOptions<'_>,
) -> Result<BundlePublishResult> {
    let bundle_path = bundle_path
        .canonicalize()
        .unwrap_or_else(|_| bundle_path.to_path_buf());
    if !bundle_path.is_dir() {
        return Err(EngitError::Publish(format!(
            "Bundle path does not exist: '{}'",
            bundle_path.display()
        )));
    }
    validate_version(options.version)?;
    if options.force && options.version != DEV_VERSION {
        return Err(EngitError::Publish(format!(
            "--force may only be used with --version {DEV_VERSION}."
        )));
    }

    let bundle_name = repo_name_from(&bundle_path);
    let bndlid = bndlid_from_name(&bundle_name);
    let bndl_name = bndlid
        .split_once(':')
        .map_or_else(|| bndlid.clone(), |(_, value)| value.to_string());
    let pub_path = publish_path(&bundle_name);
    let files = list_publish_files(
        &bundle_path,
        options.version,
        options.extra_includes,
        options.extra_excludes,
    )?;
    let directory = options.output_dir.join(pub_path).join(options.version);
    let archive = options
        .output_dir
        .join(format!("{bundle_name}-{}.zip", options.version));
    ensure_destination_available(&directory, options.version, options.force)?;
    ensure_destination_available(&archive, options.version, options.force)?;

    if options.dry_run {
        println!("Bundle: {bndlid}  version: {}", options.version);
        println!("Folder: {}", directory.display());
        if options.create_zip {
            println!("Zip:    {}", archive.display());
        }
        println!("Files that would be included ({}):", files.len());
        for (_, rel) in &files {
            println!("  {}", rel.display());
        }
        return Ok(BundlePublishResult {
            directory,
            archive: options.create_zip.then_some(archive),
        });
    }

    fs::create_dir_all(options.output_dir)
        .map_err(|source| EngitError::io(options.output_dir, source))?;
    let output_dir = options
        .output_dir
        .canonicalize()
        .unwrap_or_else(|_| options.output_dir.to_path_buf());
    let directory = output_dir
        .join(publish_path(&bundle_name))
        .join(options.version);
    let archive = output_dir.join(format!("{bundle_name}-{}.zip", options.version));
    let unique = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let staging_dir = output_dir.join(format!(".engit-{}-{unique}.dir", std::process::id()));
    let staging_zip = output_dir.join(format!(".engit-{}-{unique}.zip", std::process::id()));
    let marker =
        serde_json::to_vec_pretty(&bundle_marker_data(&bndlid, &bndl_name, options.version))
            .expect("bundle marker serialization must succeed");

    let publish_result = (|| {
        build_publish_dir(&staging_dir, &files, &marker)?;
        if options.create_zip {
            build_publish_zip(&staging_zip, &bundle_name, options.version, &files, &marker)?;
        }

        if options.force {
            remove_existing(&directory)?;
            remove_existing(&archive)?;
        }
        if let Some(parent) = directory.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| EngitError::io(parent.to_path_buf(), source))?;
        }
        fs::rename(&staging_dir, &directory)
            .map_err(|source| EngitError::io(directory.clone(), source))?;
        if options.create_zip {
            if let Err(source) = fs::rename(&staging_zip, &archive) {
                let _ = fs::remove_dir_all(&directory);
                return Err(EngitError::io(archive.clone(), source));
            }
        }
        Ok(BundlePublishResult {
            directory: directory.clone(),
            archive: options.create_zip.then_some(archive.clone()),
        })
    })();

    if publish_result.is_err() {
        let _ = fs::remove_dir_all(&staging_dir);
        let _ = fs::remove_file(&staging_zip);
    }
    publish_result
}

fn validate_version(version: &str) -> Result<()> {
    let mut components = Path::new(version).components();
    if version.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(EngitError::Publish(format!(
            "Publish version must be one safe path component: {version:?}."
        )));
    }
    Ok(())
}

fn ensure_destination_available(path: &Path, version: &str, force: bool) -> Result<()> {
    if path.exists() && !(version == DEV_VERSION && force) {
        return Err(EngitError::Publish(format!(
            "Published version already exists and is immutable: '{}'.",
            path.display()
        )));
    }
    Ok(())
}

fn remove_existing(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|source| EngitError::io(path.to_path_buf(), source))?;
    } else if path.exists() {
        fs::remove_file(path).map_err(|source| EngitError::io(path.to_path_buf(), source))?;
    }
    Ok(())
}

fn build_publish_dir(
    destination: &Path,
    files: &[(PathBuf, PathBuf)],
    marker: &[u8],
) -> Result<()> {
    fs::create_dir_all(destination)
        .map_err(|source| EngitError::io(destination.to_path_buf(), source))?;

    for (source, relative) in files {
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| EngitError::io(parent.to_path_buf(), source))?;
        }
        fs::copy(source, &target).map_err(|error| EngitError::io(target, error))?;
    }

    let marker_path = destination.join(BUNDLE_MARKER_FILE);
    fs::write(&marker_path, marker).map_err(|source| EngitError::io(marker_path, source))?;
    Ok(())
}

fn build_publish_zip(
    zip_path: &Path,
    bundle_name: &str,
    version: &str,
    files: &[(PathBuf, PathBuf)],
    marker: &[u8],
) -> Result<()> {
    let file =
        File::create(zip_path).map_err(|source| EngitError::io(zip_path.to_path_buf(), source))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let publish_prefix = bundle_name.replace('-', "/");

    for (source, relative) in files {
        let arc_name = format!(
            "{publish_prefix}/{version}/{}",
            relative.to_string_lossy().replace('\\', "/")
        );
        writer
            .start_file(&arc_name, options)
            .map_err(|source| EngitError::Publish(source.to_string()))?;
        let contents = fs::read(source).map_err(|error| EngitError::io(source.clone(), error))?;
        use std::io::Write;
        writer
            .write_all(&contents)
            .map_err(|source| EngitError::Publish(source.to_string()))?;
    }

    let marker_arc = format!("{publish_prefix}/{version}/{BUNDLE_MARKER_FILE}");
    writer
        .start_file(&marker_arc, options)
        .map_err(|source| EngitError::Publish(source.to_string()))?;
    use std::io::Write;
    writer
        .write_all(marker)
        .map_err(|source| EngitError::Publish(source.to_string()))?;
    writer
        .finish()
        .map_err(|source| EngitError::Publish(source.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::{tempdir, TempDir};
    use zip::ZipArchive;

    use super::{
        base_version, bndlid_from_name, bundle_publish, default_bundle_publish_root,
        detect_version, is_bndlid, list_publish_files, publish_path, resolve_asset_tokens,
        resolve_bndlid_to_path, BundlePublishOptions, BundlePublishResult, BUNDLE_ENV_DIR,
        BUNDLE_MARKER_FILE, BUNDLE_PUBLISH_ROOT_VAR, LEGACY_BUNDLE_ARTIFACTS_FILE,
        LEGACY_BUNDLE_PUBLISH_ROOT_VAR, PUBLISH_MANIFEST_FILE,
    };
    use crate::{error::Result, ENVOY_ENV_MUTEX};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        fs::write(path, contents).expect("failed to write test file");
    }

    fn create_bundle(name: &str) -> (TempDir, PathBuf) {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join(name);
        fs::create_dir_all(bundle.join(BUNDLE_ENV_DIR)).expect("failed to create bundle");
        (temp, bundle)
    }

    fn write_manifest(bundle: &Path, contents: &str) {
        write_file(
            &bundle.join(BUNDLE_ENV_DIR).join(PUBLISH_MANIFEST_FILE),
            contents,
        );
    }

    fn relative_files(bundle: &Path, includes: &[String], excludes: &[String]) -> Vec<String> {
        list_publish_files(bundle, "dev", includes, excludes)
            .expect("publish files should resolve")
            .into_iter()
            .map(|(_, relative)| relative.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn publish_bundle(
        bundle: &Path,
        output: &Path,
        version: &str,
        create_zip: bool,
        force: bool,
        dry_run: bool,
    ) -> Result<BundlePublishResult> {
        bundle_publish(
            bundle,
            &BundlePublishOptions {
                output_dir: output,
                version,
                create_zip,
                extra_includes: &[],
                extra_excludes: &[],
                force,
                dry_run,
            },
        )
    }

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

    fn init_repo(root: &Path) {
        run_git(&["init"], root);
        run_git(&["config", "user.name", "Engit Test"], root);
        run_git(&["config", "user.email", "engit@example.com"], root);
        write_file(&root.join("py/module.py"), "value = 1\n");
        run_git(&["add", "."], root);
        run_git(&["commit", "-m", "Initial commit"], root);
    }

    #[test]
    fn bundle_id_helpers_match_python_behavior() {
        assert_eq!(bndlid_from_name("gt-ext-python"), "gt:ext:python");
        assert_eq!(
            publish_path("gt-ext-python"),
            Path::new("gt").join("ext").join("python")
        );
        assert!(is_bndlid("gt:pythoncore"));
        assert!(!is_bndlid(r"C:\path"));
    }

    #[test]
    fn resolves_asset_tokens_and_base_version() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env_guard = EnvVarGuard::set("ENGIT_TEST_ARTIFACTS", Some(OsStr::new("R:\\assets")));

        assert_eq!(base_version("3.11.9-envoy.2"), "3.11.9");
        assert_eq!(
            resolve_asset_tokens(
                "${ENGIT_TEST_ARTIFACTS}/${BASE_VERSION}/${VERSION}",
                "3.11.9-envoy.2",
            ),
            r"R:\assets/3.11.9/3.11.9-envoy.2"
        );
    }

    #[test]
    fn default_allowlist_selects_runtime_and_legal_files() {
        let (_temp, bundle) = create_bundle("gt-runtime");
        write_manifest(&bundle, "{}\n");
        for relative in [
            ".envoy/commands.json",
            "py/module.py",
            "bin/tool.exe",
            "prebuilt/runtime.pyd",
            "resources/icon.png",
            "resource/schema.json",
            "docs/guide.md",
            "LICENSE",
            "NOTICE.txt",
            "THIRD_PARTY_LICENSES.md",
        ] {
            write_file(&bundle.join(relative), "runtime");
        }
        for relative in [
            "README.md",
            "src/source.rs",
            "scripts/build.ps1",
            "build/generated.bin",
            "py/__pycache__/module.pyc",
            "prebuilt/target/debug.obj",
        ] {
            write_file(&bundle.join(relative), "excluded");
        }

        let files = relative_files(&bundle, &[], &[]);
        for expected in [
            ".envoy/commands.json",
            ".envoy/publish-manifest.yaml",
            "py/module.py",
            "bin/tool.exe",
            "prebuilt/runtime.pyd",
            "resources/icon.png",
            "resource/schema.json",
            "docs/guide.md",
            "LICENSE",
            "NOTICE.txt",
            "THIRD_PARTY_LICENSES.md",
        ] {
            assert!(files.contains(&expected.to_string()), "missing {expected}");
        }
        for unexpected in [
            "README.md",
            "src/source.rs",
            "scripts/build.ps1",
            "build/generated.bin",
            "py/__pycache__/module.pyc",
            "prebuilt/target/debug.obj",
        ] {
            assert!(
                !files.contains(&unexpected.to_string()),
                "unexpected {unexpected}"
            );
        }
    }

    #[test]
    fn manifest_and_cli_rules_extend_defaults_with_excludes_winning() {
        let (_temp, bundle) = create_bundle("gt-rules");
        write_manifest(
            &bundle,
            "include:\n  - custom/**\nexclude:\n  - py/private/**\n",
        );
        for relative in [
            "py/public/module.py",
            "py/private/secret.py",
            "docs/guide.md",
            "custom/data.bin",
            "extra/runtime.dat",
        ] {
            write_file(&bundle.join(relative), "data");
        }

        let includes = vec![String::from("extra/**")];
        let excludes = vec![String::from("docs/**")];
        let files = relative_files(&bundle, &includes, &excludes);

        assert!(files.contains(&String::from("py/public/module.py")));
        assert!(files.contains(&String::from("custom/data.bin")));
        assert!(files.contains(&String::from("extra/runtime.dat")));
        assert!(!files.contains(&String::from("py/private/secret.py")));
        assert!(!files.contains(&String::from("docs/guide.md")));
    }

    #[test]
    fn hard_exclusions_override_an_explicit_catch_all_include() {
        let (_temp, bundle) = create_bundle("gt-hard-excludes");
        for relative in [
            "runtime.dat",
            ".git/config",
            ".gitignore",
            ".hg/store/data",
            ".svn/entries",
            "build/generated.bin",
            "target/debug/output.bin",
            "py/__pycache__/module.pyc",
        ] {
            write_file(&bundle.join(relative), "data");
        }

        let files = relative_files(&bundle, &[String::from("**")], &[]);
        assert!(files.contains(&String::from("runtime.dat")));
        for excluded in [
            ".git/config",
            ".gitignore",
            ".hg/store/data",
            ".svn/entries",
            "build/generated.bin",
            "target/debug/output.bin",
            "py/__pycache__/module.pyc",
        ] {
            assert!(
                !files.contains(&excluded.to_string()),
                "included {excluded}"
            );
        }
    }

    #[test]
    fn external_sources_use_defaults_and_source_specific_rules() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let (temp, bundle) = create_bundle("gt-external");
        let artifacts = temp.path().join("artifacts");
        let _env_guard = EnvVarGuard::set("ENGIT_TEST_ARTIFACTS", Some(artifacts.as_os_str()));
        for relative in [
            "prebuilt/runtime.dll",
            "build/generated.dll",
            "custom/extra.dat",
            "custom/skip.dat",
            "source/source.cpp",
        ] {
            write_file(&artifacts.join(relative), "artifact");
        }
        write_manifest(
            &bundle,
            "artifacts:\n  - source: ${ENGIT_TEST_ARTIFACTS}\n    destination: external\n    include:\n      - custom/**\n    exclude:\n      - custom/skip.dat\n",
        );

        let files = relative_files(&bundle, &[], &[]);
        assert!(files.contains(&String::from("external/prebuilt/runtime.dll")));
        assert!(files.contains(&String::from("external/custom/extra.dat")));
        assert!(!files.contains(&String::from("external/custom/skip.dat")));
        assert!(!files.contains(&String::from("external/build/generated.dll")));
        assert!(!files.contains(&String::from("external/source/source.cpp")));
    }

    #[test]
    fn rejects_legacy_manifest_and_unknown_yaml_keys() {
        let (_temp, bundle) = create_bundle("gt-invalid");
        write_file(
            &bundle
                .join(BUNDLE_ENV_DIR)
                .join(LEGACY_BUNDLE_ARTIFACTS_FILE),
            "{}",
        );
        let error =
            list_publish_files(&bundle, "dev", &[], &[]).expect_err("legacy manifest should fail");
        assert!(error.to_string().contains("no longer supported"));

        fs::remove_file(
            bundle
                .join(BUNDLE_ENV_DIR)
                .join(LEGACY_BUNDLE_ARTIFACTS_FILE),
        )
        .expect("legacy manifest should be removed");
        write_manifest(&bundle, "unknown: true\n");
        let error =
            list_publish_files(&bundle, "dev", &[], &[]).expect_err("unknown YAML key should fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unresolved_tokens_unsafe_destinations_and_unmatched_includes() {
        let (_temp, bundle) = create_bundle("gt-validation");
        write_manifest(
            &bundle,
            "artifacts:\n  - source: ${ENGIT_MISSING_ARTIFACT_ROOT}\n",
        );
        let error =
            list_publish_files(&bundle, "dev", &[], &[]).expect_err("missing token should fail");
        assert!(error.to_string().contains("ENGIT_MISSING_ARTIFACT_ROOT"));

        write_manifest(
            &bundle,
            "artifacts:\n  - source: .\n    destination: ../escape\n",
        );
        let error = list_publish_files(&bundle, "dev", &[], &[])
            .expect_err("unsafe destination should fail");
        assert!(error.to_string().contains("relative path"));

        write_manifest(&bundle, "{}\n");
        let includes = vec![String::from("missing/**")];
        let error = list_publish_files(&bundle, "dev", &includes, &[])
            .expect_err("unmatched include should fail");
        assert!(error.to_string().contains("matched no files"));
    }

    #[test]
    fn rejects_destination_collisions() {
        let (temp, bundle) = create_bundle("gt-collision");
        let artifacts = temp.path().join("artifacts");
        write_file(&bundle.join("py/module.py"), "bundle");
        write_file(&artifacts.join("py/module.py"), "artifact");
        write_manifest(
            &bundle,
            &format!(
                "artifacts:\n  - source: '{}'\n    destination: .\n",
                artifacts.display()
            ),
        );

        let error =
            list_publish_files(&bundle, "dev", &[], &[]).expect_err("collision should fail");
        assert!(error.to_string().contains("destination collision"));
    }

    #[test]
    fn publishes_folder_and_optional_zip_from_the_same_dataset() {
        let (_temp, bundle) = create_bundle("gt-ext-python");
        let output = bundle
            .parent()
            .expect("bundle should have parent")
            .join("out");
        write_manifest(&bundle, "{}\n");
        write_file(&bundle.join("py/module.py"), "runtime");

        let result = publish_bundle(&bundle, &output, "1.2.3", true, false, false)
            .expect("publish should succeed");
        assert!(result.directory.join("py/module.py").is_file());
        assert!(result.directory.join(BUNDLE_MARKER_FILE).is_file());
        assert!(result
            .directory
            .join(BUNDLE_ENV_DIR)
            .join(PUBLISH_MANIFEST_FILE)
            .is_file());

        let zip_path = result.archive.expect("zip should be generated");
        let file = File::open(zip_path).expect("failed to open zip");
        let mut archive = ZipArchive::new(file).expect("failed to read zip");
        archive
            .by_name("gt/ext/python/1.2.3/py/module.py")
            .expect("runtime file should exist in zip");
        archive
            .by_name("gt/ext/python/1.2.3/.envoy/publish-manifest.yaml")
            .expect("manifest should exist in zip");
        archive
            .by_name("gt/ext/python/1.2.3/.bundle")
            .expect("marker should exist in zip");
    }

    #[test]
    fn released_versions_are_immutable_and_dev_requires_force() {
        let (_temp, bundle) = create_bundle("gt-immutable");
        let output = bundle
            .parent()
            .expect("bundle should have parent")
            .join("out");
        write_file(&bundle.join("py/module.py"), "one");

        publish_bundle(&bundle, &output, "1.0.0", false, false, false)
            .expect("first release should succeed");
        let error = publish_bundle(&bundle, &output, "1.0.0", false, false, false)
            .expect_err("released version should be immutable");
        assert!(error.to_string().contains("immutable"));

        publish_bundle(&bundle, &output, "dev", false, false, false)
            .expect("first dev publish should succeed");
        write_file(&bundle.join("py/module.py"), "two");
        let error = publish_bundle(&bundle, &output, "dev", false, false, false)
            .expect_err("dev replacement should require force");
        assert!(error.to_string().contains("immutable"));

        let result = publish_bundle(&bundle, &output, "dev", false, true, false)
            .expect("forced dev replacement should succeed");
        assert_eq!(
            fs::read_to_string(result.directory.join("py/module.py"))
                .expect("published module should be readable"),
            "two"
        );
    }

    #[test]
    fn dry_run_validates_without_writing() {
        let (_temp, bundle) = create_bundle("gt-dry-run");
        let output = bundle
            .parent()
            .expect("bundle should have parent")
            .join("out");
        write_file(&bundle.join("py/module.py"), "runtime");

        let result = publish_bundle(&bundle, &output, "1.0.0", true, false, true)
            .expect("dry run should succeed");
        assert!(!result.directory.exists());
        assert!(!result
            .archive
            .expect("zip path should be reported")
            .exists());
        assert!(!output.exists());
    }

    #[test]
    fn bundle_publish_root_prefers_new_environment_variable() {
        let _lock = ENVOY_ENV_MUTEX.lock().expect("env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let preferred = temp.path().join("preferred");
        let legacy = temp.path().join("legacy");
        let _preferred_guard =
            EnvVarGuard::set(BUNDLE_PUBLISH_ROOT_VAR, Some(preferred.as_os_str()));
        let _legacy_guard =
            EnvVarGuard::set(LEGACY_BUNDLE_PUBLISH_ROOT_VAR, Some(legacy.as_os_str()));

        assert_eq!(
            default_bundle_publish_root().expect("publish root should resolve"),
            preferred
        );
    }

    #[test]
    fn resolves_bundle_id_paths_from_env() {
        let _lock = ENVOY_ENV_MUTEX
            .lock()
            .expect("bundle roots env mutex poisoned");
        let temp = tempdir().expect("failed to create temp dir");
        let root = temp.path().join("root");
        let bundle = root.join("gt").join("pythoncore");
        fs::create_dir_all(&bundle).expect("failed to create bundle path");
        let joined = std::env::join_paths([root.as_path()]).expect("failed to join paths");
        let _env_guard = EnvVarGuard::set("ENVOY_BNDL_ROOTS", Some(joined.as_os_str()));

        assert_eq!(
            resolve_bndlid_to_path("gt:pythoncore").expect("bundle path should resolve"),
            bundle
                .canonicalize()
                .expect("bundle path should canonicalize")
        );
    }

    #[test]
    fn detect_version_returns_string_form_without_v_prefix() {
        let (_temp, bundle) = create_bundle("gt-version");
        init_repo(&bundle);
        run_git(&["tag", "v1.2.3"], &bundle);

        assert_eq!(
            detect_version(&bundle).expect("version should be detected"),
            "1.2.3"
        );
    }
}
