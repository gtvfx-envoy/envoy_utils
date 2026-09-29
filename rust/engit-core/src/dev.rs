//! Local cross-repo development helpers.
//!
//! Lets a developer point this workspace's `envoy-core` Cargo dependency, or
//! a freshly built Envoy Python wheel, at an in-progress local Envoy
//! checkout instead of a published release -- without a risky by-hand
//! `Cargo.toml` edit. Every link is reversible via the matching `unlink`
//! function, and all state lives under the already-gitignored
//! `rust/target/` directory so a link can never be accidentally committed.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use regex::{NoExpand, Regex};
use serde::{Deserialize, Serialize};

use crate::error::{EngitError, Result};
use crate::git::get_repo_root;
use crate::publish::{is_bndlid, resolve_bndlid_to_path};

/// Bundle ID used to resolve a local Envoy checkout when none is given.
pub const DEFAULT_ENVOY_BNDLID: &str = "gt:envoy";

fn envoy_core_dependency_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();

    // `\r?` tolerates CRLF line endings: this repo checks out with
    // core.autocrlf=true, so rust/Cargo.toml is CRLF on Windows.
    REGEX.get_or_init(|| {
        Regex::new(r"(?m)^envoy-core\s*=\s*\{[^\r\n]+\}\r?$").expect("regex must compile")
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RustLinkState {
    original_dependency_line: String,
    linked_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PythonLinkState {
    envoy_path: String,
    bundle_dir: String,
}

/// Current dev-link state for Rust and Python.
#[derive(Clone, Debug)]
pub struct DevStatus {
    /// Path of the linked local Envoy checkout, when Rust is dev-linked.
    pub rust_linked_path: Option<String>,
    /// The `envoy-core` dependency line currently in `rust/Cargo.toml` (the
    /// published pin when not linked, the local `path =` form when linked).
    pub rust_dependency_line: Option<String>,
    /// Path of the linked local Envoy checkout, when Python is dev-linked.
    pub python_linked_path: Option<String>,
}

fn strip_windows_prefix(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(text) => match text.strip_prefix(r"\\?\") {
            Some(stripped) => PathBuf::from(stripped),
            None => path,
        },
        None => path,
    }
}

fn to_manifest_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn rust_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("rust")
}

fn dev_state_dir(rust_root: &Path) -> PathBuf {
    rust_root.join("target").join("engit-dev")
}

fn rust_link_state_path(rust_root: &Path) -> PathBuf {
    dev_state_dir(rust_root).join("rust-link.json")
}

fn python_link_state_path(rust_root: &Path) -> PathBuf {
    dev_state_dir(rust_root).join("python-link.json")
}

fn python_bundle_dir(rust_root: &Path) -> PathBuf {
    dev_state_dir(rust_root).join("envoy-python")
}

fn python_wheel_dir(rust_root: &Path) -> PathBuf {
    dev_state_dir(rust_root).join("envoy-python-wheel")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let contents =
        fs::read_to_string(path).map_err(|source| EngitError::io(path.to_path_buf(), source))?;
    serde_json::from_str(&contents).map_err(|source| EngitError::json(path.to_path_buf(), source))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| EngitError::io(parent.to_path_buf(), source))?;
    }
    let contents = serde_json::to_string_pretty(value)
        .map_err(|source| EngitError::json(path.to_path_buf(), source))?;
    fs::write(path, contents).map_err(|source| EngitError::io(path.to_path_buf(), source))
}

/// Resolve a bundle-ID-or-path spec to a validated local Envoy checkout root.
fn resolve_local_envoy_path(spec: Option<&str>) -> Result<PathBuf> {
    let spec = spec.unwrap_or(DEFAULT_ENVOY_BNDLID);
    let candidate = if is_bndlid(spec) {
        resolve_bndlid_to_path(spec)?
    } else {
        PathBuf::from(spec)
    };
    let canonical = fs::canonicalize(&candidate)
        .map(strip_windows_prefix)
        .map_err(|source| {
            EngitError::Dev(format!(
                "Could not resolve an Envoy checkout at '{spec}' ({}): {source}",
                candidate.display()
            ))
        })?;
    if !canonical
        .join("rust")
        .join("envoy-core")
        .join("Cargo.toml")
        .is_file()
    {
        return Err(EngitError::Dev(format!(
            "'{}' does not look like an Envoy checkout (missing \
rust/envoy-core/Cargo.toml).",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn run_tool<I, S>(program: &str, args: I, cwd: &Path) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|value| value.as_ref().to_string_lossy().into_owned())
        .collect();
    let mut command = Command::new(program);
    command.args(&args);
    command.current_dir(cwd);

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            EngitError::Dev(format!("'{program}' executable not found on PATH."))
        } else {
            EngitError::io(cwd.to_path_buf(), source)
        }
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        return Err(EngitError::tool_command(program, &args, &stdout, &stderr));
    }

    Ok(stdout)
}

/// Run the `envoy` launcher, tolerating a repo-local `envoy.bat`/`envoy.cmd`
/// dev wrapper on Windows.
///
/// `std::process::Command` does not perform the `PATHEXT`-style resolution
/// a shell does: it will not find `envoy.bat` when asked to spawn plain
/// `envoy`, even though the same name works from an interactive prompt.
/// Real installed releases ship a native `envoy`/`envoy.exe`, so this only
/// matters for contributors running against a source checkout -- exactly
/// the audience for `engit dev link python`.
fn run_envoy<I, S>(args: I, cwd: &Path) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if cfg!(windows) {
        let mut full_args: Vec<String> = vec![String::from("/C"), String::from("envoy")];
        full_args.extend(
            args.into_iter()
                .map(|value| value.as_ref().to_string_lossy().into_owned()),
        );
        run_tool("cmd", full_args, cwd)
    } else {
        run_tool("envoy", args, cwd)
    }
}

/// Swap `rust/Cargo.toml`'s `envoy-core` dependency to a local path,
/// preserving the original pin in `state_path` if not already saved.
fn swap_dependency_to_path(
    manifest_path: &Path,
    state_path: &Path,
    envoy_core_manifest_path: &str,
    linked_display_path: &str,
) -> Result<()> {
    let contents = fs::read_to_string(manifest_path)
        .map_err(|source| EngitError::io(manifest_path.to_path_buf(), source))?;
    let regex = envoy_core_dependency_regex();
    let match_count = regex.find_iter(&contents).count();
    if match_count != 1 {
        return Err(EngitError::Dev(format!(
            "Expected exactly one envoy-core dependency line in {}; found {match_count}.",
            manifest_path.display()
        )));
    }
    let current_line = regex
        .find(&contents)
        .expect("match_count == 1 guarantees a match")
        .as_str()
        .to_string();
    // Preserve this line's own CRLF-vs-LF ending so the swap never mixes
    // line-ending styles within the file (this repo checks out CRLF).
    let line_ending = if current_line.ends_with('\r') {
        "\r"
    } else {
        ""
    };

    let original_line = if state_path.is_file() {
        read_json::<RustLinkState>(state_path)?.original_dependency_line
    } else if current_line.contains("path =") {
        return Err(EngitError::Dev(String::from(
            "rust/Cargo.toml already declares a local path dependency for \
envoy-core, but no engit dev-link state was found to recover the original \
pin. Restore the git/tag dependency manually before running `engit dev \
link rust` again.",
        )));
    } else {
        current_line
    };

    let new_line =
        format!(r#"envoy-core = {{ path = "{envoy_core_manifest_path}" }}{line_ending}"#);
    let updated = regex
        .replace(&contents, NoExpand(new_line.as_str()))
        .into_owned();
    fs::write(manifest_path, updated)
        .map_err(|source| EngitError::io(manifest_path.to_path_buf(), source))?;

    write_json(
        state_path,
        &RustLinkState {
            original_dependency_line: original_line,
            linked_path: linked_display_path.to_string(),
        },
    )
}

/// Restore `rust/Cargo.toml`'s `envoy-core` dependency from saved state.
/// Returns `false` when nothing was linked.
fn restore_dependency_from_state(manifest_path: &Path, state_path: &Path) -> Result<bool> {
    if !state_path.is_file() {
        return Ok(false);
    }
    let state = read_json::<RustLinkState>(state_path)?;
    let contents = fs::read_to_string(manifest_path)
        .map_err(|source| EngitError::io(manifest_path.to_path_buf(), source))?;
    let regex = envoy_core_dependency_regex();
    let match_count = regex.find_iter(&contents).count();
    if match_count != 1 {
        return Err(EngitError::Dev(format!(
            "Expected exactly one envoy-core dependency line in {}; found {match_count}.",
            manifest_path.display()
        )));
    }
    let updated = regex
        .replace(&contents, NoExpand(state.original_dependency_line.as_str()))
        .into_owned();
    fs::write(manifest_path, updated)
        .map_err(|source| EngitError::io(manifest_path.to_path_buf(), source))?;
    fs::remove_file(state_path)
        .map_err(|source| EngitError::io(state_path.to_path_buf(), source))?;
    Ok(true)
}

/// Link this workspace's `envoy-core` dependency to a local Envoy checkout.
///
/// Returns the resolved local Envoy checkout path.
pub fn run_dev_link_rust(envoy_spec: Option<&str>, cwd: Option<&Path>) -> Result<PathBuf> {
    let repo_root = get_repo_root(cwd)?;
    let rust_root = rust_dir(&repo_root);
    let manifest_path = rust_root.join("Cargo.toml");
    let state_path = rust_link_state_path(&rust_root);
    let envoy_path = resolve_local_envoy_path(envoy_spec)?;
    let envoy_core_path = to_manifest_path(&envoy_path.join("rust").join("envoy-core"));

    swap_dependency_to_path(
        &manifest_path,
        &state_path,
        &envoy_core_path,
        &envoy_path.display().to_string(),
    )?;

    run_tool("cargo", ["check", "--workspace"], &rust_root)?;

    Ok(envoy_path)
}

/// Restore the published `envoy-core` dependency. Returns `false` when Rust
/// was not currently dev-linked.
pub fn run_dev_unlink_rust(cwd: Option<&Path>) -> Result<bool> {
    let repo_root = get_repo_root(cwd)?;
    let rust_root = rust_dir(&repo_root);
    let manifest_path = rust_root.join("Cargo.toml");
    let state_path = rust_link_state_path(&rust_root);

    if !restore_dependency_from_state(&manifest_path, &state_path)? {
        return Ok(false);
    }

    run_tool("cargo", ["check", "--workspace"], &rust_root)?;

    Ok(true)
}

/// Write the generated dev bundle's `.envoy/python_env.json`.
fn write_dev_python_bundle_env(bundle_dir: &Path) -> Result<()> {
    let envoy_dir = bundle_dir.join(".envoy");
    fs::create_dir_all(&envoy_dir).map_err(|source| EngitError::io(envoy_dir.clone(), source))?;
    let env_json = envoy_dir.join("python_env.json");
    fs::write(
        &env_json,
        "{\n    \"+=PYTHONPATH\": \"${__BUNDLE__}/site-packages\"\n}\n",
    )
    .map_err(|source| EngitError::io(env_json.clone(), source))
}

/// Parse `envoy --which python` output into an interpreter path.
///
/// The documented form is a single bare path. When resolution passes
/// through a command alias, `envoy` instead prints a multi-line, human
/// readable explanation containing a `resolved to: <path>` line; prefer
/// that line's path when present.
fn parse_envoy_which_output(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    for line in trimmed.lines() {
        if let Some((_, after)) = line.split_once("resolved to:") {
            let candidate = after.trim();
            if !candidate.is_empty() {
                return Some(PathBuf::from(candidate));
            }
        }
    }
    let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
    if first_line.is_empty() {
        None
    } else {
        Some(PathBuf::from(first_line))
    }
}

/// Build a local Envoy Python wheel and install it into an isolated,
/// self-contained Envoy bundle directory (a `.envoy/python_env.json`
/// pointing `PYTHONPATH` at the installed wheel). No shared/global
/// site-packages are ever touched.
///
/// Returns the generated bundle directory. Add its parent directory to
/// `ENVOY_BNDL_ROOTS`, or reference it directly, to use it.
pub fn run_dev_link_python(
    envoy_spec: Option<&str>,
    release: bool,
    cwd: Option<&Path>,
) -> Result<PathBuf> {
    let repo_root = get_repo_root(cwd)?;
    let rust_root = rust_dir(&repo_root);
    let envoy_path = resolve_local_envoy_path(envoy_spec)?;
    let envoy_py_manifest = envoy_path.join("rust").join("envoy-py").join("Cargo.toml");
    if !envoy_py_manifest.is_file() {
        return Err(EngitError::Dev(format!(
            "'{}' does not contain rust/envoy-py/Cargo.toml.",
            envoy_path.display()
        )));
    }

    let which_output = run_envoy(["--which", "python"], &rust_root)?;
    let python_path = parse_envoy_which_output(&which_output).ok_or_else(|| {
        EngitError::Dev(String::from(
            "'envoy --which python' returned no interpreter path. Is a Stack \
with a Python bundle active?",
        ))
    })?;
    let python = python_path.to_string_lossy().into_owned();

    let bundle_dir = python_bundle_dir(&rust_root);
    let wheel_dir = python_wheel_dir(&rust_root);
    let site_packages = bundle_dir.join("site-packages");
    for stale in [&bundle_dir, &wheel_dir] {
        if stale.is_dir() {
            fs::remove_dir_all(stale).map_err(|source| EngitError::io(stale.clone(), source))?;
        }
    }
    fs::create_dir_all(&wheel_dir).map_err(|source| EngitError::io(wheel_dir.clone(), source))?;
    fs::create_dir_all(&site_packages)
        .map_err(|source| EngitError::io(site_packages.clone(), source))?;

    let mut build_args = vec![
        String::from("build"),
        String::from("--manifest-path"),
        to_manifest_path(&envoy_py_manifest),
        String::from("--out"),
        to_manifest_path(&wheel_dir),
        String::from("--interpreter"),
        python.clone(),
    ];
    if release {
        build_args.push(String::from("--release"));
    }
    run_tool("maturin", build_args, &rust_root)?;

    let wheel_path = fs::read_dir(&wheel_dir)
        .map_err(|source| EngitError::io(wheel_dir.clone(), source))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("whl"))
        .ok_or_else(|| {
            EngitError::Dev(format!(
                "maturin did not produce a .whl file in {}.",
                wheel_dir.display()
            ))
        })?;

    let install_args = vec![
        String::from("-m"),
        String::from("pip"),
        String::from("install"),
        String::from("--disable-pip-version-check"),
        String::from("--no-deps"),
        String::from("--force-reinstall"),
        String::from("--target"),
        to_manifest_path(&site_packages),
        to_manifest_path(&wheel_path),
    ];
    run_tool(&python, install_args, &rust_root)?;

    write_dev_python_bundle_env(&bundle_dir)?;

    write_json(
        &python_link_state_path(&rust_root),
        &PythonLinkState {
            envoy_path: envoy_path.display().to_string(),
            bundle_dir: bundle_dir.display().to_string(),
        },
    )?;

    Ok(bundle_dir)
}

/// Remove the generated local Python dev bundle. Returns `false` when
/// Python was not currently dev-linked.
pub fn run_dev_unlink_python(cwd: Option<&Path>) -> Result<bool> {
    let repo_root = get_repo_root(cwd)?;
    let rust_root = rust_dir(&repo_root);
    let state_path = python_link_state_path(&rust_root);
    if !state_path.is_file() {
        return Ok(false);
    }
    let state = read_json::<PythonLinkState>(&state_path)?;
    let bundle_dir = PathBuf::from(&state.bundle_dir);
    if bundle_dir.is_dir() {
        fs::remove_dir_all(&bundle_dir)
            .map_err(|source| EngitError::io(bundle_dir.clone(), source))?;
    }
    let wheel_dir = python_wheel_dir(&rust_root);
    if wheel_dir.is_dir() {
        fs::remove_dir_all(&wheel_dir)
            .map_err(|source| EngitError::io(wheel_dir.clone(), source))?;
    }
    fs::remove_file(&state_path)
        .map_err(|source| EngitError::io(state_path.to_path_buf(), source))?;
    Ok(true)
}

pub(crate) fn format_dev_status_lines(status: &DevStatus) -> Vec<String> {
    let mut lines = Vec::new();

    match &status.rust_linked_path {
        Some(path) => lines.push(format!("Rust:   DEV-LINKED -> {path}")),
        None => lines.push(format!(
            "Rust:   {}",
            status
                .rust_dependency_line
                .as_deref()
                .unwrap_or("(envoy-core dependency not found)")
        )),
    }
    match &status.python_linked_path {
        Some(path) => lines.push(format!("Python: DEV-LINKED -> {path}")),
        None => lines.push(String::from("Python: pinned (no local dev bundle)")),
    }

    lines
}

fn collect_dev_status(cwd: Option<&Path>) -> Result<DevStatus> {
    let repo_root = get_repo_root(cwd)?;
    let rust_root = rust_dir(&repo_root);
    let manifest_path = rust_root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest_path)
        .map_err(|source| EngitError::io(manifest_path.clone(), source))?;
    let current_line = envoy_core_dependency_regex()
        .find(&contents)
        .map(|found| found.as_str().to_string());

    let rust_state_path = rust_link_state_path(&rust_root);
    let (rust_linked_path, rust_dependency_line) = if rust_state_path.is_file() {
        let state = read_json::<RustLinkState>(&rust_state_path)?;
        (
            Some(state.linked_path),
            Some(state.original_dependency_line),
        )
    } else {
        (None, current_line)
    };

    let python_state_path = python_link_state_path(&rust_root);
    let python_linked_path = if python_state_path.is_file() {
        Some(read_json::<PythonLinkState>(&python_state_path)?.envoy_path)
    } else {
        None
    };

    Ok(DevStatus {
        rust_linked_path,
        rust_dependency_line,
        python_linked_path,
    })
}

/// Print local dev-link status for Rust and Python.
pub fn run_dev_status(cwd: Option<&Path>) -> Result<()> {
    let status = collect_dev_status(cwd)?;
    for line in format_dev_status_lines(&status) {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{
        format_dev_status_lines, parse_envoy_which_output, read_json,
        restore_dependency_from_state, swap_dependency_to_path, write_dev_python_bundle_env,
        DevStatus, RustLinkState,
    };

    const SAMPLE_MANIFEST: &str = "[workspace]\n\
resolver = \"2\"\n\
\n\
[workspace.dependencies]\n\
clap = { version = \"4\" }\n\
envoy-core = { git = \"https://github.com/gtvfx-envoy/envoy\", tag = \"v0.6.2\", version = \"=0.6.2\" }\n\
serde = { version = \"1\" }\n";

    #[test]
    fn swap_and_restore_round_trip_preserves_original_pin() {
        let dir = tempdir().expect("temp dir");
        let manifest_path = dir.path().join("Cargo.toml");
        let state_path = dir
            .path()
            .join("target")
            .join("engit-dev")
            .join("rust-link.json");
        fs::write(&manifest_path, SAMPLE_MANIFEST).expect("write manifest");

        swap_dependency_to_path(
            &manifest_path,
            &state_path,
            "/checkout/rust/envoy-core",
            "/checkout",
        )
        .expect("swap should succeed");

        let linked_contents = fs::read_to_string(&manifest_path).expect("read manifest");
        assert!(linked_contents.contains(r#"envoy-core = { path = "/checkout/rust/envoy-core" }"#));
        assert!(state_path.is_file());

        // Re-linking to a different path must not lose the original pin.
        swap_dependency_to_path(
            &manifest_path,
            &state_path,
            "/other/rust/envoy-core",
            "/other",
        )
        .expect("re-link should succeed");
        let state = read_json::<RustLinkState>(&state_path).expect("read state");
        assert!(state.original_dependency_line.contains(r#"tag = "v0.6.2""#));

        let restored =
            restore_dependency_from_state(&manifest_path, &state_path).expect("restore succeeds");
        assert!(restored);
        let restored_contents = fs::read_to_string(&manifest_path).expect("read manifest");
        assert!(restored_contents.contains(r#"tag = "v0.6.2""#));
        assert!(!state_path.is_file());
    }

    #[test]
    fn swap_and_restore_round_trip_handles_crlf_line_endings() {
        // This repo checks out with core.autocrlf=true, so rust/Cargo.toml
        // is CRLF on Windows; the regex and swap must not corrupt that.
        let dir = tempdir().expect("temp dir");
        let manifest_path = dir.path().join("Cargo.toml");
        let state_path = dir
            .path()
            .join("target")
            .join("engit-dev")
            .join("rust-link.json");
        let crlf_manifest = SAMPLE_MANIFEST.replace('\n', "\r\n");
        fs::write(&manifest_path, &crlf_manifest).expect("write manifest");

        swap_dependency_to_path(
            &manifest_path,
            &state_path,
            "/checkout/rust/envoy-core",
            "/checkout",
        )
        .expect("swap should succeed on a CRLF manifest");

        let linked_contents = fs::read_to_string(&manifest_path).expect("read manifest");
        assert!(
            linked_contents.contains("envoy-core = { path = \"/checkout/rust/envoy-core\" }\r\n")
        );
        // Every other line must keep its original CRLF ending untouched.
        assert!(linked_contents.contains("clap = { version = \"4\" }\r\n"));

        let restored =
            restore_dependency_from_state(&manifest_path, &state_path).expect("restore succeeds");
        assert!(restored);
        let restored_contents = fs::read_to_string(&manifest_path).expect("read manifest");
        assert_eq!(restored_contents, crlf_manifest);
    }

    #[test]
    fn restore_without_prior_link_is_a_no_op() {
        let dir = tempdir().expect("temp dir");
        let manifest_path = dir.path().join("Cargo.toml");
        let state_path = dir
            .path()
            .join("target")
            .join("engit-dev")
            .join("rust-link.json");
        fs::write(&manifest_path, SAMPLE_MANIFEST).expect("write manifest");

        let restored =
            restore_dependency_from_state(&manifest_path, &state_path).expect("no-op restore");

        assert!(!restored);
    }

    #[test]
    fn parse_envoy_which_output_accepts_the_documented_bare_path_form() {
        let parsed = parse_envoy_which_output("C:\\Python311\\python.exe\n");

        assert_eq!(parsed, Some(PathBuf::from("C:\\Python311\\python.exe")));
    }

    #[test]
    fn parse_envoy_which_output_prefers_the_resolved_to_line_when_aliased() {
        let raw = "command python aliased to: ${__BUNDLE__}/prebuilt/python311/python.exe\r\n\
command python resolved to: V:\\repo\\gtvfx-envoy\\ext\\python\\prebuilt\\python311\\python.exe\r\n\
  defined in: V:\\repo\\gtvfx-envoy\\ext\\python\\.envoy\\commands.json\r\n";

        let parsed = parse_envoy_which_output(raw);

        assert_eq!(
            parsed,
            Some(PathBuf::from(
                "V:\\repo\\gtvfx-envoy\\ext\\python\\prebuilt\\python311\\python.exe"
            ))
        );
    }

    #[test]
    fn parse_envoy_which_output_rejects_blank_output() {
        assert_eq!(parse_envoy_which_output("   \n  "), None);
    }

    #[test]
    fn swap_rejects_uncommitted_state_when_manifest_already_local() {
        let dir = tempdir().expect("temp dir");
        let manifest_path = dir.path().join("Cargo.toml");
        let state_path = dir
            .path()
            .join("target")
            .join("engit-dev")
            .join("rust-link.json");
        let manifest_with_local_path = SAMPLE_MANIFEST.replace(
            r#"envoy-core = { git = "https://github.com/gtvfx-envoy/envoy", tag = "v0.6.2", version = "=0.6.2" }"#,
            r#"envoy-core = { path = "/already/linked" }"#,
        );
        fs::write(&manifest_path, manifest_with_local_path).expect("write manifest");

        let error = swap_dependency_to_path(&manifest_path, &state_path, "/checkout", "/checkout")
            .expect_err("swap without recoverable state should fail");

        assert!(error
            .to_string()
            .contains("no engit dev-link state was found"));
    }

    #[test]
    fn format_dev_status_lines_reports_linked_and_pinned_states() {
        let linked = DevStatus {
            rust_linked_path: Some(String::from("/checkout")),
            rust_dependency_line: None,
            python_linked_path: None,
        };
        let pinned = DevStatus {
            rust_linked_path: None,
            rust_dependency_line: Some(String::from(
                "envoy-core = { git = \"...\", tag = \"v0.6.2\", version = \"=0.6.2\" }",
            )),
            python_linked_path: Some(String::from("/checkout")),
        };

        assert_eq!(
            format_dev_status_lines(&linked)[0],
            "Rust:   DEV-LINKED -> /checkout"
        );
        assert_eq!(
            format_dev_status_lines(&linked)[1],
            "Python: pinned (no local dev bundle)"
        );
        assert!(format_dev_status_lines(&pinned)[0].starts_with("Rust:   envoy-core"));
        assert_eq!(
            format_dev_status_lines(&pinned)[1],
            "Python: DEV-LINKED -> /checkout"
        );
    }

    #[test]
    fn write_dev_python_bundle_env_writes_bundle_token_pythonpath() {
        let dir = tempdir().expect("temp dir");

        write_dev_python_bundle_env(dir.path()).expect("write bundle env");

        let contents = fs::read_to_string(dir.path().join(".envoy").join("python_env.json"))
            .expect("read env json");
        assert!(contents.contains("${__BUNDLE__}/site-packages"));
    }
}
