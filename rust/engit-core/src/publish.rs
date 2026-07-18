//! Bundle publish helpers.

use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::{EngitError, Result};
use crate::git::{get_latest_semver_tag, get_remote_url, is_git_repo};

/// Version string used for local test builds.
pub const DEV_VERSION: &str = "dev";
/// Marker file written at the root of every published bundle.
pub const BUNDLE_MARKER_FILE: &str = ".bundle";
/// Artifact-source config filename under `.envoy/`.
pub const BUNDLE_ARTIFACTS_FILE: &str = "bundle-artifacts.json";
/// Per-bundle envoy configuration directory.
pub const BUNDLE_ENV_DIR: &str = ".envoy";
/// Patterns excluded from every publish.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".gitignore",
    ".github",
    "build",
    "dist",
    ".pytest_cache",
    "__pycache__",
    "*.pyc",
    "*.pyo",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct BundleArtifact {
    source: PathBuf,
    dest: PathBuf,
}

#[derive(Serialize)]
struct BundleMarkerData<'a> {
    bndlid: &'a str,
    name: &'a str,
    version: &'a str,
    published: String,
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

fn load_bundle_artifacts(
    bundle_path: &Path,
    version: &str,
) -> (Vec<BundleArtifact>, Vec<String>) {
    let artifacts_file = bundle_path.join(BUNDLE_ENV_DIR).join(BUNDLE_ARTIFACTS_FILE);
    if !artifacts_file.is_file() {
        return (Vec::new(), Vec::new());
    }

    let Ok(contents) = fs::read_to_string(&artifacts_file) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(data) = serde_json::from_str::<Value>(&contents) else {
        return (Vec::new(), Vec::new());
    };

    let mut resolved = Vec::new();
    if let Some(artifacts) = data.get("artifacts").and_then(Value::as_array) {
        for artifact in artifacts {
            let Some(source) = artifact.get("source").and_then(Value::as_str) else {
                return (Vec::new(), Vec::new());
            };
            let dest = artifact.get("dest").and_then(Value::as_str).unwrap_or(".");
            resolved.push(BundleArtifact {
                source: PathBuf::from(resolve_asset_tokens(source, version)),
                dest: PathBuf::from(dest),
            });
        }
    }

    let excludes: Vec<String> = data
        .get("exclude")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    (resolved, excludes)
}

fn glob_matches(name: &str, pattern: &str) -> bool {
    let mut regex = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' => regex.push_str(r"\."),
            '\\' => regex.push_str(r"\\"),
            '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' => {
                regex.push('\\');
                regex.push(ch);
            }
            other => regex.push(other),
        }
    }
    regex.push('$');

    Regex::new(&regex)
        .expect("generated glob regex must compile")
        .is_match(name)
}

fn is_excluded(name: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|pattern| glob_matches(name, pattern))
}

fn combined_excludes(extra_excludes: Option<&[String]>) -> Vec<String> {
    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    if let Some(extra_excludes) = extra_excludes {
        excludes.extend(extra_excludes.iter().cloned());
    }
    excludes
}

fn collect_files_recursive(
    current: &Path,
    base: &Path,
    excludes: &[String],
    collected: &mut Vec<PathBuf>,
) {
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    let mut entries: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();

    for path in entries {
        let Some(name) = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
        else {
            continue;
        };
        if is_excluded(&name, excludes) {
            continue;
        }
        if path.is_dir() {
            collect_files_recursive(&path, base, excludes, collected);
        } else if path.is_file() {
            collected.push(path.strip_prefix(base).map_or(path.clone(), PathBuf::from));
        }
    }
}

fn collect_files(bundle_path: &Path, excludes: &[String]) -> Vec<PathBuf> {
    let mut collected = Vec::new();
    collect_files_recursive(bundle_path, bundle_path, excludes, &mut collected);
    collected.sort();
    collected
}

fn collect_artifact_files(artifact_source: &Path, excludes: &[String]) -> Vec<(PathBuf, PathBuf)> {
    let mut relative_files = collect_files(artifact_source, excludes);
    relative_files.sort();
    relative_files
        .drain(..)
        .map(|relative| (artifact_source.join(&relative), relative))
        .collect()
}

/// Return the list of files that would be included in a publish.
pub fn list_publish_files(
    bundle_path: &Path,
    version: &str,
    extra_excludes: Option<&[String]>,
) -> Vec<(PathBuf, PathBuf)> {
    let bundle_path = bundle_path
        .canonicalize()
        .unwrap_or_else(|_| bundle_path.to_path_buf());

    // Merge excludes from bundle-artifacts.json with CLI-provided ones.
    let (artifacts, file_excludes) = load_bundle_artifacts(&bundle_path, version);
    let mut merged_excludes: Vec<String> = file_excludes;
    if let Some(cli_excludes) = extra_excludes {
        merged_excludes.extend(cli_excludes.iter().cloned());
    }
    let excludes = combined_excludes(Some(merged_excludes.as_slice()));

    let mut files: Vec<(PathBuf, PathBuf)> = collect_files(&bundle_path, &excludes)
        .into_iter()
        .map(|relative| (bundle_path.join(&relative), relative))
        .collect();

    for artifact in artifacts {
        if !artifact.source.is_dir() {
            continue;
        }
        for (src, rel) in collect_artifact_files(&artifact.source, &excludes) {
            let rel_in_bundle = if artifact.dest == Path::new(".") {
                rel
            } else {
                artifact.dest.join(rel)
            };
            files.push((src, rel_in_bundle));
        }
    }

    files
}

/// Create a versioned publish of `bundle_path`.
pub fn bundle_publish(
    bundle_path: &Path,
    output_dir: &Path,
    version: &str,
    zip_mode: bool,
    extra_excludes: Option<&[String]>,
    dry_run: bool,
) -> Result<PathBuf> {
    let bundle_path = bundle_path
        .canonicalize()
        .unwrap_or_else(|_| bundle_path.to_path_buf());
    if !bundle_path.is_dir() {
        return Err(EngitError::Publish(format!(
            "Bundle path does not exist: '{}'",
            bundle_path.display()
        )));
    }

    let bundle_name = repo_name_from(&bundle_path);
    let bndlid = bndlid_from_name(&bundle_name);
    let bndl_name = bndlid
        .split_once(':')
        .map_or_else(|| bndlid.clone(), |(_, value)| value.to_string());
    let pub_path = publish_path(&bundle_name);
    let files = list_publish_files(&bundle_path, version, extra_excludes);

    if dry_run {
        println!("Bundle: {bndlid}  version: {version}");
        println!("Mode:   {}", if zip_mode { "zip" } else { "folder" });
        println!("Files that would be included ({}):", files.len());
        for (_, rel) in &files {
            println!("  {}", rel.display());
        }
        return Ok(output_dir.join(pub_path).join(version));
    }

    let output_dir = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    fs::create_dir_all(&output_dir).map_err(|source| EngitError::io(output_dir.clone(), source))?;

    if zip_mode {
        build_publish_zip(
            &bundle_name,
            &bndlid,
            &bndl_name,
            version,
            &files,
            &output_dir,
        )
    } else {
        build_publish_dir(
            &bundle_name,
            &bndlid,
            &bndl_name,
            version,
            &files,
            &output_dir,
        )
    }
}

fn write_marker(path: &Path, bndlid: &str, bndl_name: &str, version: &str) -> Result<()> {
    let marker = serde_json::to_string_pretty(&bundle_marker_data(bndlid, bndl_name, version))
        .expect("bundle marker serialization must succeed");

    fs::write(path, marker).map_err(|source| EngitError::io(path.to_path_buf(), source))
}

fn build_publish_dir(
    bundle_name: &str,
    bndlid: &str,
    bndl_name: &str,
    version: &str,
    files: &[(PathBuf, PathBuf)],
    output_dir: &Path,
) -> Result<PathBuf> {
    let dest_root = output_dir.join(publish_path(bundle_name)).join(version);
    if dest_root.exists() {
        fs::remove_dir_all(&dest_root)
            .map_err(|source| EngitError::io(dest_root.clone(), source))?;
    }
    fs::create_dir_all(&dest_root).map_err(|source| EngitError::io(dest_root.clone(), source))?;

    for (src, rel) in files {
        let dest = dest_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| EngitError::io(parent.to_path_buf(), source))?;
        }
        fs::copy(src, &dest).map_err(|source| EngitError::io(dest.clone(), source))?;
    }

    write_marker(
        &dest_root.join(BUNDLE_MARKER_FILE),
        bndlid,
        bndl_name,
        version,
    )?;
    Ok(dest_root)
}

fn build_publish_zip(
    bundle_name: &str,
    bndlid: &str,
    bndl_name: &str,
    version: &str,
    files: &[(PathBuf, PathBuf)],
    output_dir: &Path,
) -> Result<PathBuf> {
    let zip_path = output_dir.join(format!("{bundle_name}-{version}.zip"));
    let file =
        File::create(&zip_path).map_err(|source| EngitError::io(zip_path.clone(), source))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let publish_prefix = bundle_name.replace('-', "/");

    for (src, rel) in files {
        let arc_name = format!(
            "{publish_prefix}/{version}/{}",
            rel.to_string_lossy().replace('\\', "/")
        );
        writer
            .start_file(&arc_name, options)
            .map_err(|source| EngitError::Publish(source.to_string()))?;
        let contents = fs::read(src).map_err(|source| EngitError::io(src.clone(), source))?;
        use std::io::Write;
        writer
            .write_all(&contents)
            .map_err(|source| EngitError::Publish(source.to_string()))?;
    }

    let marker_arc = format!("{publish_prefix}/{version}/{BUNDLE_MARKER_FILE}");
    writer
        .start_file(&marker_arc, options)
        .map_err(|source| EngitError::Publish(source.to_string()))?;
    let marker = serde_json::to_vec_pretty(&bundle_marker_data(bndlid, bndl_name, version))
        .expect("bundle marker serialization must succeed");
    use std::io::Write;
    writer
        .write_all(&marker)
        .map_err(|source| EngitError::Publish(source.to_string()))?;
    writer
        .finish()
        .map_err(|source| EngitError::Publish(source.to_string()))?;

    Ok(zip_path)
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File};
    use std::path::Path;
    use std::process::Command;
    use std::sync::Mutex;

    use tempfile::tempdir;
    use zip::ZipArchive;

    use super::{
        base_version, bndlid_from_name, bundle_publish, detect_version, is_bndlid,
        list_publish_files, publish_path, resolve_asset_tokens, resolve_bndlid_to_path,
        BUNDLE_ARTIFACTS_FILE, BUNDLE_ENV_DIR, BUNDLE_MARKER_FILE,
    };

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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
        fs::write(root.join("file.txt"), "one\n").expect("failed to write file");
        fs::create_dir_all(root.join(BUNDLE_ENV_DIR)).expect("failed to create .envoy dir");
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
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env_guard = EnvVarGuard::set("ENVOY_TEST_ARTIFACTS", Some(OsStr::new("R:\\assets")));

        assert_eq!(base_version("3.11.9-envoy.2"), "3.11.9");
        assert_eq!(
            resolve_asset_tokens(
                "${ENVOY_TEST_ARTIFACTS}/${BASE_VERSION}/${VERSION}",
                "3.11.9-envoy.2",
            ),
            r"R:\assets/3.11.9/3.11.9-envoy.2"
        );
    }

    #[test]
    fn lists_publish_files_and_artifacts() {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("gt-ext-python");
        let artifacts = temp.path().join("artifacts");
        fs::create_dir_all(bundle.join(BUNDLE_ENV_DIR)).expect("failed to create .envoy dir");
        fs::create_dir_all(bundle.join(".git")).expect("failed to create .git dir");
        fs::create_dir_all(&artifacts).expect("failed to create artifact dir");
        fs::write(bundle.join("keep.txt"), "keep").expect("failed to write file");
        fs::write(bundle.join(".gitignore"), "ignored").expect("failed to write file");
        fs::write(artifacts.join("asset.txt"), "asset").expect("failed to write asset");
        fs::write(
            bundle.join(BUNDLE_ENV_DIR).join(BUNDLE_ARTIFACTS_FILE),
            serde_json::json!({
                "artifacts": [
                    {
                        "source": artifacts.display().to_string(),
                        "dest": "external",
                    }
                ]
            })
            .to_string(),
        )
        .expect("failed to write artifact config");

        let files = list_publish_files(&bundle, "dev", None);
        let rels = files
            .into_iter()
            .map(|(_, rel)| rel.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();

        assert!(rels.contains(&String::from("keep.txt")));
        assert!(rels.contains(&String::from(".envoy/bundle-artifacts.json")));
        assert!(rels.contains(&String::from("external/asset.txt")));
        assert!(!rels.contains(&String::from(".gitignore")));
    }

    #[test]
    fn excludes_from_bundle_artifacts_json() {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("gt-ext-python");
        fs::create_dir_all(bundle.join(BUNDLE_ENV_DIR)).expect("failed to create .envoy dir");
        fs::write(bundle.join("keep.txt"), "keep").expect("failed to write file");
        fs::write(bundle.join("skip-me.txt"), "skip").expect("failed to write file");
        fs::create_dir_all(bundle.join("excluded-dir")).expect("failed to create excluded dir");
        fs::write(
            bundle.join("excluded-dir/inner.txt"),
            "inside",
        )
        .expect("failed to write inner file");
        fs::write(
            bundle.join(BUNDLE_ENV_DIR).join(BUNDLE_ARTIFACTS_FILE),
            serde_json::json!({
                "exclude": ["skip-me.txt", "excluded-dir"]
            })
            .to_string(),
        )
        .expect("failed to write artifact config");

        let files = list_publish_files(&bundle, "dev", None);
        let rels: Vec<String> = files
            .into_iter()
            .map(|(_, rel)| rel.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(rels.contains(&String::from("keep.txt")));
        assert!(!rels.contains(&String::from("skip-me.txt")));
        assert!(!rels.contains(&String::from("excluded-dir/inner.txt")));
    }

    #[test]
    fn excludes_from_bundle_artifacts_json_merged_with_cli() {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("gt-ext-python");
        fs::create_dir_all(bundle.join(BUNDLE_ENV_DIR)).expect("failed to create .envoy dir");
        fs::write(bundle.join("keep.txt"), "keep").expect("failed to write file");
        fs::write(bundle.join("from-json.txt"), "json-exclude").expect("failed to write file");
        fs::write(
            bundle.join("from-cli.txt"),
            "cli-exclude",
        )
        .expect("failed to write file");
        fs::write(
            bundle.join(BUNDLE_ENV_DIR).join(BUNDLE_ARTIFACTS_FILE),
            serde_json::json!({
                "exclude": ["from-json.txt"]
            })
            .to_string(),
        )
        .expect("failed to write artifact config");

        let cli_excludes = vec![String::from("from-cli.txt")];
        let files = list_publish_files(&bundle, "dev", Some(cli_excludes.as_slice()));
        let rels: Vec<String> = files
            .into_iter()
            .map(|(_, rel)| rel.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(rels.contains(&String::from("keep.txt")));
        assert!(!rels.contains(&String::from("from-json.txt")));
        assert!(!rels.contains(&String::from("from-cli.txt")));
    }

    #[test]
    fn publishes_directory_and_zip_outputs() {
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("gt-ext-python");
        let output = temp.path().join("out");
        fs::create_dir_all(bundle.join(BUNDLE_ENV_DIR)).expect("failed to create .envoy dir");
        fs::write(bundle.join("keep.txt"), "keep").expect("failed to write file");

        let folder = bundle_publish(&bundle, &output, "1.2.3", false, None, false)
            .expect("folder publish should succeed");
        assert!(folder.join("keep.txt").is_file());
        assert!(folder.join(BUNDLE_MARKER_FILE).is_file());

        let zip_path = bundle_publish(&bundle, &output, "1.2.3", true, None, false)
            .expect("zip publish should succeed");
        let file = File::open(&zip_path).expect("failed to open zip");
        let mut archive = ZipArchive::new(file).expect("failed to read zip");
        {
            let entry = archive
                .by_name("gt/ext/python/1.2.3/keep.txt")
                .expect("keep.txt should exist in zip");
            assert_eq!(entry.name(), "gt/ext/python/1.2.3/keep.txt");
        }
        archive
            .by_name("gt/ext/python/1.2.3/.bundle")
            .expect(".bundle marker should exist in zip");
    }

    #[test]
    fn resolves_bundle_id_paths_from_env() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
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
        let temp = tempdir().expect("failed to create temp dir");
        let bundle = temp.path().join("gt-ext-python");
        fs::create_dir_all(&bundle).expect("failed to create bundle dir");
        init_repo(&bundle);
        run_git(&["tag", "v1.2.3"], &bundle);

        assert_eq!(
            detect_version(&bundle).expect("version should be detected"),
            "1.2.3"
        );
    }
}
