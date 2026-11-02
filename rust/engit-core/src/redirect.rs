//! Publishing and reading the redirect file that pins an Envoy version for each user.
//!
//! The launcher reads this file from the config root on the share before it
//! starts the app, so a bad file stops everyone it covers from starting Envoy.
//! Publishing therefore checks the file with the launcher's own code, checks
//! that every pinned version can be found, keeps a copy of everything it
//! publishes, and replaces the live file in one step.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use envoy_core::config::expand::expand_source;
use envoy_core::config::locate::{CONFIG_ROOT_VAR, REDIRECT_FILE_NAME};
use envoy_core::config::redirect::{normalize_user, Redirect, Severity};

use crate::error::{EngitError, Result};

/// The config root `engit` publishes into.
///
/// The launcher reads `ENVOY_CONFIG_PROD` instead. It is deliberately not used
/// to publish, so that a consumer's environment cannot publish by accident.
pub const CONFIG_PUBLISH_ROOT_VAR: &str = "ENVOY_CONFIG_PUBLISH_ROOT";

const HISTORY_DIR: &str = "history";
const HISTORY_PREFIX: &str = "redirect-";
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d-%H%M%S";
const TIMESTAMP_LENGTH: usize = 17;

/// A redirect file is a few lines. Anything bigger is the wrong file.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What the launcher needs to find in a folder for it to be an Envoy version.
const APP_EXECUTABLES: [&str; 2] = ["bin/envoy.exe", "bin/envoy"];

type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// What [`publish_redirect`] is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct PublishOptions<'a> {
    /// The redirect file to publish.
    pub source: &'a Path,
    /// The config root, or `ENVOY_CONFIG_PUBLISH_ROOT` when `None`.
    pub output: Option<&'a Path>,
    /// Publish for one team, into its own folder under the root.
    pub team: Option<&'a str>,
    /// Refuse a file that pins a version no source has.
    pub check_sources: bool,
    /// Check everything and say what would be written, without writing it.
    pub dry_run: bool,
}

/// What a publish did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The live file was replaced.
    Published,
    /// A dry run: it would have been replaced.
    WouldPublish,
    /// The live file already has exactly this content.
    Unchanged,
}

/// Whether a pinned version can be found on a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// The folder that holds it.
    Found(PathBuf),
    /// Every place that was tried, and why it did not have the version.
    Missing(Vec<String>),
    /// The sources were not looked at.
    NotChecked,
}

/// A group of the redirect file and the version it is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedVersion {
    /// The group's name.
    pub group: String,
    /// The version the group runs.
    pub version: String,
    /// Where the version can be found.
    pub availability: Availability,
}

/// The result of [`publish_redirect`].
#[derive(Debug)]
pub struct PublishReport {
    /// What happened.
    pub outcome: Outcome,
    /// The file the launcher reads.
    pub live: PathBuf,
    /// The copy kept in the history, when there is one.
    pub history: Option<PathBuf>,
    /// Every group and the version it is pinned to.
    pub pins: Vec<PinnedVersion>,
}

impl fmt::Display for PublishReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let heading = match self.outcome {
            Outcome::Published => "Published the redirect file.",
            Outcome::WouldPublish => "Would publish the redirect file.",
            Outcome::Unchanged => "Nothing to publish: the live file already has this content.",
        };
        writeln!(formatter, "{heading}")?;
        writeln!(formatter, "  Live:     {}", self.live.display())?;
        if let Some(history) = &self.history {
            writeln!(formatter, "  History:  {}", history.display())?;
        }
        writeln!(formatter, "  Versions:")?;
        write_pins(formatter, &self.pins)
    }
}

fn write_pins(formatter: &mut fmt::Formatter<'_>, pins: &[PinnedVersion]) -> fmt::Result {
    let width = pins.iter().map(|pin| pin.group.len()).max().unwrap_or(0);
    for pin in pins {
        write!(formatter, "    {:<width$}  {}", pin.group, pin.version)?;
        match &pin.availability {
            Availability::Found(folder) => {
                writeln!(formatter, "  (found at {})", folder.display())?
            }
            Availability::Missing(_) => writeln!(formatter, "  (NOT FOUND on any source)")?,
            Availability::NotChecked => writeln!(formatter, "  (sources not checked)")?,
        }
    }
    Ok(())
}

/// Check a redirect file and publish it.
///
/// The file is parsed and validated with the launcher's own code, and, unless
/// told not to, every version it pins must be found on a source. Nothing is
/// written when a check fails. A file that is already live is left alone.
///
/// Otherwise a copy is kept in the history first, so that every file that was
/// ever live can be put back, and then the live file is replaced in one step: a
/// launcher reading it sees the old file or the new one, never half of either.
pub fn publish_redirect(options: &PublishOptions<'_>) -> Result<PublishReport> {
    let stamp = Utc::now().format(TIMESTAMP_FORMAT).to_string();
    publish_with(options, &|name| env::var(name).ok(), &stamp)
}

fn publish_with(options: &PublishOptions<'_>, env: &Env, stamp: &str) -> Result<PublishReport> {
    let root = publish_root(options.output, env)?;
    // A mistyped root must not become a new folder that no launcher reads.
    if !root.is_dir() {
        return Err(EngitError::Redirect(format!(
            "{} is not a folder. The config root must already exist: check the path.",
            root.display()
        )));
    }
    let directory = redirect_directory(&root, options.team)?;
    let live = directory.join(REDIRECT_FILE_NAME);

    let (text, bytes) = read_source(options.source)?;
    let redirect = Redirect::parse(&text)
        .map_err(|error| EngitError::Redirect(format!("{}: {error}", options.source.display())))?;
    check_problems(options.source, &redirect)?;

    let pins = pinned_versions(&redirect, options.check_sources, env);
    if options.check_sources {
        check_found(&pins)?;
    }

    if is_same_file(options.source, &live) {
        return Err(EngitError::Redirect(format!(
            "{} is the live file itself. Publish a copy of it.",
            live.display()
        )));
    }

    let history = directory
        .join(HISTORY_DIR)
        .join(history_name(stamp, &publisher(env)));
    let unchanged = fs::read(&live).is_ok_and(|current| current == bytes);
    let report = |outcome, history| PublishReport {
        outcome,
        live: live.clone(),
        history,
        pins: pins.clone(),
    };

    if unchanged {
        return Ok(report(Outcome::Unchanged, None));
    }
    if options.dry_run {
        return Ok(report(Outcome::WouldPublish, Some(history)));
    }

    write_files(&directory, &live, &history, &bytes)?;
    Ok(report(Outcome::Published, Some(history)))
}

/// Keep the history copy, then replace the live file.
fn write_files(directory: &Path, live: &Path, history: &Path, bytes: &[u8]) -> Result<()> {
    let history_dir = directory.join(HISTORY_DIR);
    fs::create_dir_all(&history_dir).map_err(|source| EngitError::io(&history_dir, source))?;
    if history.exists() {
        return Err(EngitError::Redirect(format!(
            "{} already exists. Someone published at the same second: try again.",
            history.display()
        )));
    }

    stage_and_move(&history_dir, history, bytes, false)?;
    if let Err(error) = stage_and_move(directory, live, bytes, true) {
        // Every history entry was live once, so one that never was must go.
        let _ = fs::remove_file(history);
        return Err(error);
    }

    match fs::read(live) {
        Ok(found) if found == bytes => Ok(()),
        Ok(_) => Err(EngitError::Redirect(format!(
            "{} does not have the content that was just published. Is someone else publishing?",
            live.display()
        ))),
        Err(source) => Err(EngitError::io(live, source)),
    }
}

/// Write `bytes` beside `target` and move them into place.
fn stage_and_move(directory: &Path, target: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let unique = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staged = directory.join(format!(".{name}.{}-{unique}.tmp", std::process::id()));

    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    if let Err(source) = written {
        let _ = fs::remove_file(&staged);
        return Err(EngitError::io(&staged, source));
    }

    let moved = if replace {
        rename_with_retry(&staged, target)
    } else {
        fs::rename(&staged, target)
    };
    moved.map_err(|source| {
        let _ = fs::remove_file(&staged);
        EngitError::io(target, source)
    })
}

/// Rename `from` over `to`, retrying while another program has `to` open.
///
/// The launcher reads the file each time it starts, so a publish can land while
/// it is open.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 10;

    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 < ATTEMPTS && is_busy(&error) => {
                attempt += 1;
                thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
            }
            Err(error) => return Err(error),
        }
    }
}

/// `true` when another program has the file open (Windows).
fn is_busy(error: &io::Error) -> bool {
    // SHARING_VIOLATION, LOCK_VIOLATION
    cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

fn read_source(path: &Path) -> Result<(String, Vec<u8>)> {
    let metadata = fs::metadata(path).map_err(|source| EngitError::io(path, source))?;
    if !metadata.is_file() {
        return Err(EngitError::Redirect(format!(
            "{} is not a file.",
            path.display()
        )));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(EngitError::Redirect(format!(
            "{} is {} bytes, far more than a redirect file. Is it the right file?",
            path.display(),
            metadata.len()
        )));
    }

    let bytes = fs::read(path).map_err(|source| EngitError::io(path, source))?;
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| EngitError::Redirect(format!("{} is not UTF-8 text.", path.display())))?;
    Ok((text, bytes))
}

fn check_problems(source: &Path, redirect: &Redirect) -> Result<()> {
    let errors: Vec<String> = redirect
        .validate()
        .into_iter()
        .filter(|problem| problem.severity == Severity::Error)
        .map(|problem| problem.message)
        .collect();
    if errors.is_empty() {
        return Ok(());
    }

    let list: String = errors
        .iter()
        .map(|message| format!("\n  - {message}"))
        .collect();
    Err(EngitError::Redirect(format!(
        "{} has {} error(s) and was not published:{list}",
        source.display(),
        errors.len()
    )))
}

fn pinned_versions(redirect: &Redirect, check_sources: bool, env: &Env) -> Vec<PinnedVersion> {
    redirect
        .versions
        .iter()
        .map(|(group, version)| PinnedVersion {
            group: group.clone(),
            version: version.clone(),
            availability: if check_sources {
                find_app(version, &redirect.sources, env)
            } else {
                Availability::NotChecked
            },
        })
        .collect()
}

fn check_found(pins: &[PinnedVersion]) -> Result<()> {
    let missing: Vec<String> = pins
        .iter()
        .filter_map(|pin| match &pin.availability {
            Availability::Missing(tried) => Some(format!(
                "group '{}' is pinned to {}, which was not found. Looked in:\n    {}",
                pin.group,
                pin.version,
                tried.join("\n    ")
            )),
            _ => None,
        })
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    Err(EngitError::Redirect(format!(
        "{} pinned version(s) cannot be found on any source, so the file was not published:\n  {}\n\
Fix the file or the sources, or publish anyway with --no-check-sources.",
        missing.len(),
        missing.join("\n  ")
    )))
}

/// Look for a version the way the launcher does: the first source with the app in it.
fn find_app(version: &str, sources: &[String], env: &Env) -> Availability {
    let mut tried = Vec::new();
    for template in sources {
        match expand_source(template, version, env) {
            Ok(folder) => {
                if APP_EXECUTABLES
                    .iter()
                    .any(|relative| folder.join(relative).is_file())
                {
                    return Availability::Found(folder);
                }
                tried.push(format!(
                    "{}: there is no {} there",
                    folder.display(),
                    APP_EXECUTABLES.join(" or ")
                ));
            }
            Err(error) => tried.push(error.to_string()),
        }
    }
    Availability::Missing(tried)
}

fn is_same_file(first: &Path, second: &Path) -> bool {
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

fn setting(env: &Env, name: &str) -> Option<String> {
    env(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn publish_root(output: Option<&Path>, env: &Env) -> Result<PathBuf> {
    if let Some(path) = output {
        return Ok(path.to_path_buf());
    }
    setting(env, CONFIG_PUBLISH_ROOT_VAR)
        .map(PathBuf::from)
        .ok_or_else(|| {
            EngitError::Redirect(format!(
                "No --output specified and {CONFIG_PUBLISH_ROOT_VAR} is not set."
            ))
        })
}

/// The root to read from. Reading cannot change anything, so the variable the
/// launcher reads is allowed here too.
fn read_root(root: Option<&Path>, env: &Env) -> Result<PathBuf> {
    if let Some(path) = root {
        return Ok(path.to_path_buf());
    }
    setting(env, CONFIG_PUBLISH_ROOT_VAR)
        .or_else(|| setting(env, CONFIG_ROOT_VAR))
        .map(PathBuf::from)
        .ok_or_else(|| {
            EngitError::Redirect(format!(
                "No --root specified and neither {CONFIG_PUBLISH_ROOT_VAR} nor {CONFIG_ROOT_VAR} is set."
            ))
        })
}

/// The folder that holds the redirect file: the root, or a team's folder in it.
fn redirect_directory(root: &Path, team: Option<&str>) -> Result<PathBuf> {
    match team {
        None => Ok(root.to_path_buf()),
        Some(team) if is_safe_team(team) => Ok(root.join(team)),
        Some(team) => Err(EngitError::Redirect(format!(
            "'{team}' cannot be a team name: use letters, digits, '.', '_' and '-'."
        ))),
    }
}

/// The launcher's rule for a team name, which becomes a folder name.
fn is_safe_team(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.chars().all(|c| c == '.')
}

/// Who is publishing, as the name in a history entry.
fn publisher(env: &Env) -> String {
    let raw = setting(env, "USERNAME")
        .or_else(|| setting(env, "USER"))
        .unwrap_or_default();
    let name: String = normalize_user(&raw)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

fn history_name(stamp: &str, user: &str) -> String {
    format!("{HISTORY_PREFIX}{stamp}-{user}.yaml")
}

/// What [`show_redirect`] is asked to show.
#[derive(Debug, Clone, Copy)]
pub struct ShowOptions<'a> {
    /// The config root, or `ENVOY_CONFIG_PUBLISH_ROOT`, then `ENVOY_CONFIG_PROD`, when `None`.
    pub root: Option<&'a Path>,
    /// Show a team's file instead of everyone's.
    pub team: Option<&'a str>,
    /// Say which version this user gets.
    pub user: Option<&'a str>,
}

/// The live redirect file, read back.
#[derive(Debug)]
pub struct ShowReport {
    /// The file the launcher reads.
    pub live: PathBuf,
    /// The schema it was written with.
    pub schema: u32,
    /// Where versions are looked for, as written.
    pub sources: Vec<String>,
    /// Every group and where its version can be found.
    pub pins: Vec<PinnedVersion>,
    /// Each listed user and their group.
    pub users: BTreeMap<String, String>,
    /// The answer for the user that was asked about.
    pub resolution: Option<String>,
}

impl fmt::Display for ShowReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Redirect file: {}", self.live.display())?;
        writeln!(formatter, "  Schema:   {}", self.schema)?;
        writeln!(formatter, "  Sources:")?;
        for source in &self.sources {
            writeln!(formatter, "    {source}")?;
        }
        writeln!(formatter, "  Versions:")?;
        write_pins(formatter, &self.pins)?;
        if self.users.is_empty() {
            writeln!(
                formatter,
                "  Users:    none listed; everyone gets 'default'"
            )?;
        } else {
            writeln!(formatter, "  Users:")?;
            let width = self.users.keys().map(String::len).max().unwrap_or(0);
            for (user, group) in &self.users {
                writeln!(formatter, "    {user:<width$}  {group}")?;
            }
        }
        if let Some(resolution) = &self.resolution {
            writeln!(formatter, "  {resolution}")?;
        }
        Ok(())
    }
}

/// Read the live redirect file back and say what it does.
///
/// Each version is looked for on the sources, but one that is missing is shown,
/// not an error: this is for looking at a file that is already published.
pub fn show_redirect(options: &ShowOptions<'_>) -> Result<ShowReport> {
    show_with(options, &|name| env::var(name).ok())
}

fn show_with(options: &ShowOptions<'_>, env: &Env) -> Result<ShowReport> {
    let root = read_root(options.root, env)?;
    let live = redirect_directory(&root, options.team)?.join(REDIRECT_FILE_NAME);
    let (text, _) = read_source(&live)?;
    let redirect = Redirect::parse(&text)
        .map_err(|error| EngitError::Redirect(format!("{}: {error}", live.display())))?;

    let resolution = options.user.map(|user| match redirect.resolve(user) {
        Ok(found) => format!(
            "{}: group '{}'{} -> {}",
            found.user,
            found.group,
            if found.assigned {
                " (listed)"
            } else {
                " (not listed)"
            },
            found.version
        ),
        Err(error) => format!("{}: {error}", normalize_user(user)),
    });

    Ok(ShowReport {
        schema: redirect.schema,
        sources: redirect.sources.clone(),
        pins: pinned_versions(&redirect, true, env),
        users: listed_users(&text),
        resolution,
        live,
    })
}

/// The users a file lists. The parsed file keeps them to itself, and this is
/// only for display.
fn listed_users(text: &str) -> BTreeMap<String, String> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    let mut users = BTreeMap::new();
    if let Some(listed) = value.get("users").and_then(serde_yaml::Value::as_mapping) {
        for (user, group) in listed {
            if let (Some(user), Some(group)) = (user.as_str(), group.as_str()) {
                users.insert(normalize_user(user), group.trim().to_string());
            }
        }
    }
    users
}

/// One file that was published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Where the copy is.
    pub path: PathBuf,
    /// When it was published, `YYYY-MM-DD-HHMMSS` in UTC.
    pub stamp: String,
    /// Who published it.
    pub user: String,
    /// Its size.
    pub bytes: u64,
    /// `true` when it is what the launcher reads now.
    pub live: bool,
}

/// Every file that was published, newest first.
///
/// To put one back, publish it again: `engit publish redirect <path>`.
pub fn redirect_history(root: Option<&Path>, team: Option<&str>) -> Result<Vec<HistoryEntry>> {
    history_with(root, team, &|name| env::var(name).ok())
}

fn history_with(root: Option<&Path>, team: Option<&str>, env: &Env) -> Result<Vec<HistoryEntry>> {
    let directory = redirect_directory(&read_root(root, env)?, team)?;
    let current = fs::read(directory.join(REDIRECT_FILE_NAME)).ok();

    let history_dir = directory.join(HISTORY_DIR);
    let listing = match fs::read_dir(&history_dir) {
        Ok(listing) => listing,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(EngitError::io(&history_dir, source)),
    };

    let mut entries = Vec::new();
    for item in listing {
        let item = item.map_err(|source| EngitError::io(&history_dir, source))?;
        let Some((stamp, user)) = parse_history_name(&item.file_name().to_string_lossy()) else {
            continue;
        };
        let path = item.path();
        let contents = fs::read(&path).map_err(|source| EngitError::io(&path, source))?;
        entries.push(HistoryEntry {
            live: current.as_deref() == Some(contents.as_slice()),
            bytes: contents.len() as u64,
            path,
            stamp,
            user,
        });
    }
    entries.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| a.user.cmp(&b.user)));

    // A file published again after a revert matches an older entry as well, but
    // only the newest copy is the one that went live last.
    let mut found_live = false;
    for entry in &mut entries {
        if entry.live {
            entry.live = !found_live;
            found_live = true;
        }
    }
    Ok(entries)
}

fn parse_history_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(HISTORY_PREFIX)?.strip_suffix(".yaml")?;
    let (stamp, user) = rest.split_at_checked(TIMESTAMP_LENGTH)?;
    let user = user.strip_prefix('-')?;
    let shaped = stamp.chars().enumerate().all(|(index, c)| match index {
        4 | 7 | 10 => c == '-',
        _ => c.is_ascii_digit(),
    });
    (shaped && !user.is_empty()).then(|| (stamp.to_string(), user.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::{tempdir, TempDir};

    use super::{
        history_with, parse_history_name, publish_with, show_with, Availability, Outcome,
        PublishOptions, ShowOptions, CONFIG_PUBLISH_ROOT_VAR,
    };
    use crate::error::EngitError;

    const STAMP: &str = "2026-11-02-171000";

    const FILE: &str = "\
schema: 1
sources:
  - ${STUDIO}/envoy/{version}
versions:
  default: v0.8.0
  preview: v0.8.1-rc.1
users:
  alice: preview
";

    struct Setup {
        _temp: TempDir,
        root: PathBuf,
        studio: PathBuf,
        source: PathBuf,
    }

    fn setup() -> Setup {
        let temp = tempdir().expect("tempdir is created");
        let studio = temp.path().join("bndl");
        for version in ["v0.8.0", "v0.8.1-rc.1"] {
            make_app(&studio, version);
        }
        let source = temp.path().join("redirect.yaml");
        fs::write(&source, FILE).expect("source is written");
        let root = temp.path().join("config");
        fs::create_dir_all(&root).expect("root is created");
        Setup {
            root,
            studio,
            source,
            _temp: temp,
        }
    }

    fn make_app(studio: &Path, version: &str) {
        let bin = studio.join("envoy").join(version).join("bin");
        fs::create_dir_all(&bin).expect("bin is created");
        fs::write(bin.join("envoy.exe"), "").expect("executable is written");
    }

    fn env_for(setup: &Setup) -> impl Fn(&str) -> Option<String> {
        let variables: HashMap<String, String> = HashMap::from([
            ("STUDIO".to_string(), setup.studio.display().to_string()),
            ("USERNAME".to_string(), "CORP\\Alice".to_string()),
        ]);
        move |name| variables.get(name).cloned()
    }

    fn options<'a>(setup: &'a Setup) -> PublishOptions<'a> {
        PublishOptions {
            source: &setup.source,
            output: Some(&setup.root),
            team: None,
            check_sources: true,
            dry_run: false,
        }
    }

    fn publish(
        setup: &Setup,
        options: &PublishOptions<'_>,
        stamp: &str,
    ) -> crate::Result<super::PublishReport> {
        publish_with(options, &env_for(setup), stamp)
    }

    fn message(result: crate::Result<super::PublishReport>) -> String {
        result.expect_err("the publish must fail").to_string()
    }

    /// `true` when nothing at all, not even a folder, was put in the root.
    fn untouched(root: &Path) -> bool {
        fs::read_dir(root).is_ok_and(|mut listing| listing.next().is_none())
    }

    fn files_under(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(listing) = fs::read_dir(&directory) else {
                continue;
            };
            for item in listing {
                let path = item.expect("entry reads").path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(
                        path.strip_prefix(root)
                            .expect("under root")
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn publishing_writes_the_live_file_and_a_history_copy_with_the_same_content() {
        let setup = setup();

        let report = publish(&setup, &options(&setup), STAMP).expect("publishes");

        assert_eq!(report.outcome, Outcome::Published);
        assert_eq!(fs::read_to_string(&report.live).expect("live reads"), FILE);
        let history = report.history.expect("a history copy is kept");
        assert_eq!(fs::read_to_string(&history).expect("history reads"), FILE);
        assert_eq!(
            history.file_name().and_then(|name| name.to_str()),
            Some("redirect-2026-11-02-171000-alice.yaml")
        );
        assert_eq!(report.pins.len(), 2);
        assert!(report
            .pins
            .iter()
            .all(|pin| matches!(pin.availability, Availability::Found(_))));
    }

    #[test]
    fn nothing_but_the_two_files_is_left_behind() {
        let setup = setup();

        publish(&setup, &options(&setup), STAMP).expect("publishes");

        assert_eq!(
            files_under(&setup.root),
            [
                "history/redirect-2026-11-02-171000-alice.yaml",
                "redirect.yaml"
            ]
        );
    }

    #[test]
    fn publishing_the_same_content_again_changes_nothing() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");

        let again = publish(&setup, &options(&setup), "2026-11-02-171100").expect("checks");

        assert_eq!(again.outcome, Outcome::Unchanged);
        assert!(again.history.is_none());
        assert_eq!(files_under(&setup.root).len(), 2);
    }

    #[test]
    fn a_changed_file_gets_a_new_history_entry_and_keeps_the_old_one() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");

        let report = publish(&setup, &options(&setup), "2026-11-02-171100").expect("publishes");

        assert_eq!(report.outcome, Outcome::Published);
        assert!(fs::read_to_string(&report.live)
            .expect("live reads")
            .contains("alice: default"));
        assert_eq!(files_under(&setup.root).len(), 3);
        let first = setup
            .root
            .join("history")
            .join("redirect-2026-11-02-171000-alice.yaml");
        assert_eq!(fs::read_to_string(first).expect("first reads"), FILE);
    }

    #[test]
    fn a_file_with_errors_is_refused_and_nothing_is_written() {
        let setup = setup();
        fs::write(
            &setup.source,
            "schema: 1\nsources: ['${STUDIO}/envoy/{version}']\nversions: {default: v0.8.0}\n\
users: {alice: missing}\nteam: x\n",
        )
        .expect("source is written");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains("user 'alice' is in group 'missing'"),
            "{text}"
        );
        assert!(text.contains("unknown top-level key 'team'"), "{text}");
        assert!(text.contains("2 error(s)"), "{text}");
        assert!(untouched(&setup.root), "nothing may be created");
    }

    #[test]
    fn a_syntax_error_names_the_file() {
        let setup = setup();
        fs::write(&setup.source, "schema: 1\nversions: [oops\n").expect("source is written");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(text.contains("redirect.yaml"), "{text}");
        assert!(untouched(&setup.root));
    }

    #[test]
    fn a_file_for_a_newer_schema_is_refused() {
        let setup = setup();
        fs::write(&setup.source, FILE.replace("schema: 1", "schema: 99")).expect("written");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(text.contains("99"), "{text}");
        assert!(untouched(&setup.root));
    }

    #[test]
    fn a_missing_or_wrong_source_file_is_an_error() {
        let setup = setup();
        let mut wanted = options(&setup);
        let missing = setup.studio.join("nope.yaml");
        wanted.source = &missing;
        assert!(message(publish(&setup, &wanted, STAMP)).contains("nope.yaml"));

        wanted.source = &setup.studio;
        assert!(message(publish(&setup, &wanted, STAMP)).contains("is not a file"));
        assert!(untouched(&setup.root));
    }

    #[test]
    fn a_version_nothing_provides_is_refused_unless_sources_are_not_checked() {
        let setup = setup();
        fs::write(&setup.source, FILE.replace("v0.8.1-rc.1", "v0.9.9")).expect("written");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains("group 'preview' is pinned to v0.9.9"),
            "{text}"
        );
        assert!(text.contains("v0.9.9"), "{text}");
        assert!(text.contains("--no-check-sources"), "{text}");
        assert!(untouched(&setup.root));

        let mut unchecked = options(&setup);
        unchecked.check_sources = false;
        let report = publish(&setup, &unchecked, STAMP).expect("publishes unchecked");
        assert_eq!(report.outcome, Outcome::Published);
        assert!(report
            .pins
            .iter()
            .all(|pin| pin.availability == Availability::NotChecked));
    }

    #[test]
    fn a_folder_without_the_app_is_not_a_version() {
        let setup = setup();
        fs::remove_file(setup.studio.join("envoy/v0.8.0/bin/envoy.exe")).expect("removed");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains("there is no bin/envoy.exe or bin/envoy there"),
            "{text}"
        );
    }

    #[test]
    fn a_source_that_cannot_be_expanded_says_why() {
        let setup = setup();
        fs::write(&setup.source, FILE.replace("STUDIO", "NOT_SET")).expect("written");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(text.contains("NOT_SET is not set"), "{text}");
    }

    #[test]
    fn a_dry_run_checks_everything_and_writes_nothing() {
        let setup = setup();
        let mut dry = options(&setup);
        dry.dry_run = true;

        let report = publish(&setup, &dry, STAMP).expect("checks");

        assert_eq!(report.outcome, Outcome::WouldPublish);
        assert!(report.history.is_some());
        assert!(untouched(&setup.root));

        fs::write(&setup.source, "schema: 1\n").expect("written");
        assert!(message(publish(&setup, &dry, STAMP)).contains("error(s)"));
    }

    #[test]
    fn a_team_publishes_into_its_own_folder() {
        let setup = setup();
        let mut team = options(&setup);
        team.team = Some("rigging");

        let report = publish(&setup, &team, STAMP).expect("publishes");

        assert_eq!(
            report.live,
            setup.root.join("rigging").join("redirect.yaml")
        );
        assert_eq!(
            files_under(&setup.root),
            [
                "rigging/history/redirect-2026-11-02-171000-alice.yaml",
                "rigging/redirect.yaml"
            ]
        );
    }

    #[test]
    fn a_team_name_cannot_leave_the_config_root() {
        let setup = setup();
        for name in ["..", "../other", "a/b", "a\\b", "", "."] {
            let mut team = options(&setup);
            team.team = Some(name);

            let text = message(publish(&setup, &team, STAMP));

            assert!(text.contains("cannot be a team name"), "{name:?}: {text}");
        }
        assert!(untouched(&setup.root));
    }

    #[test]
    fn a_root_that_does_not_exist_is_refused_and_not_created() {
        let setup = setup();
        let mistyped = setup.root.with_file_name("confg");
        for dry_run in [false, true] {
            let mut wanted = options(&setup);
            wanted.output = Some(&mistyped);
            wanted.dry_run = dry_run;

            let text = message(publish(&setup, &wanted, STAMP));

            assert!(text.contains("must already exist"), "{text}");
        }
        assert!(!mistyped.exists());
    }

    #[test]
    fn the_publish_root_comes_from_the_publish_variable_and_never_the_runtime_one() {
        let setup = setup();
        let mut variables: HashMap<String, String> = HashMap::from([
            ("STUDIO".to_string(), setup.studio.display().to_string()),
            (
                "ENVOY_CONFIG_PROD".to_string(),
                setup.root.display().to_string(),
            ),
        ]);
        let mut wanted = options(&setup);
        wanted.output = None;

        let lookup = |name: &str| variables.get(name).cloned();
        let text = publish_with(&wanted, &lookup, STAMP)
            .expect_err("the runtime variable must not publish")
            .to_string();
        assert!(text.contains(CONFIG_PUBLISH_ROOT_VAR), "{text}");
        assert!(untouched(&setup.root));

        variables.insert(
            CONFIG_PUBLISH_ROOT_VAR.to_string(),
            setup.root.display().to_string(),
        );
        let lookup = |name: &str| variables.get(name).cloned();
        let report = publish_with(&wanted, &lookup, STAMP).expect("publishes");
        assert_eq!(report.live, setup.root.join("redirect.yaml"));
    }

    #[test]
    fn publishing_the_live_file_onto_itself_is_refused() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        let live = setup.root.join("redirect.yaml");
        let mut itself = options(&setup);
        itself.source = &live;

        let text = message(publish(&setup, &itself, "2026-11-02-171100"));

        assert!(text.contains("is the live file itself"), "{text}");
    }

    #[test]
    fn two_publishes_in_the_same_second_do_not_replace_each_other() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(text.contains("already exists"), "{text}");
        assert!(fs::read_to_string(setup.root.join("redirect.yaml"))
            .expect("live reads")
            .contains("alice: preview"));
    }

    #[test]
    fn a_failed_replace_leaves_no_history_entry_behind() {
        let setup = setup();
        // A folder where the live file belongs makes the replace fail.
        fs::create_dir_all(setup.root.join("redirect.yaml")).expect("blocker is created");

        let error = publish(&setup, &options(&setup), STAMP).expect_err("the replace fails");

        assert!(matches!(error, EngitError::Io { .. }), "{error}");
        let history = setup.root.join("history");
        let left = fs::read_dir(&history)
            .map(|listing| listing.count())
            .unwrap_or(0);
        assert_eq!(
            left, 0,
            "a history entry that was never live must be removed"
        );
    }

    #[test]
    fn history_lists_the_newest_first_and_marks_the_live_one() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");
        publish(&setup, &options(&setup), "2026-11-02-171100").expect("publishes");
        fs::write(setup.root.join("history").join("notes.txt"), "x").expect("stray file");

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.stamp.as_str(), entry.user.as_str(), entry.live))
                .collect::<Vec<_>>(),
            [
                ("2026-11-02-171100", "alice", true),
                ("2026-11-02-171000", "alice", false)
            ]
        );
        assert!(entries.iter().all(|entry| entry.bytes > 0));
    }

    #[test]
    fn history_marks_only_the_newest_copy_of_a_file_that_was_put_back() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");
        publish(&setup, &options(&setup), "2026-11-02-171100").expect("publishes");
        fs::write(&setup.source, FILE).expect("source is put back");
        publish(&setup, &options(&setup), "2026-11-02-171200").expect("publishes");

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.stamp.as_str(), entry.live))
                .collect::<Vec<_>>(),
            [
                ("2026-11-02-171200", true),
                ("2026-11-02-171100", false),
                ("2026-11-02-171000", false)
            ]
        );
    }

    #[test]
    fn history_of_nothing_published_is_empty() {
        let setup = setup();

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");

        assert!(entries.is_empty());
    }

    #[test]
    fn history_names_are_read_back() {
        assert_eq!(
            parse_history_name("redirect-2026-11-02-171000-alice.yaml"),
            Some(("2026-11-02-171000".to_string(), "alice".to_string()))
        );
        assert_eq!(
            parse_history_name("redirect-2026-11-02-171000-a-b.c.yaml"),
            Some(("2026-11-02-171000".to_string(), "a-b.c".to_string()))
        );
        for name in [
            "redirect.yaml",
            "redirect-2026-11-02-171000.yaml",
            "redirect-2026-11-02-171000-.yaml",
            "redirect-2026x11-02-171000-alice.yaml",
            "redirect-2026-11-02-171000-alice.txt",
            "notes.txt",
        ] {
            assert_eq!(parse_history_name(name), None, "{name}");
        }
    }

    #[test]
    fn show_reports_the_file_and_what_one_user_gets() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::remove_file(setup.studio.join("envoy/v0.8.1-rc.1/bin/envoy.exe")).expect("removed");
        let wanted = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: Some("CORP\\ALICE"),
        };

        let report = show_with(&wanted, &env_for(&setup)).expect("shows");

        assert_eq!(report.schema, 1);
        assert_eq!(
            report.users.get("alice").map(String::as_str),
            Some("preview")
        );
        assert_eq!(
            report.resolution.as_deref(),
            Some("alice: group 'preview' (listed) -> v0.8.1-rc.1")
        );
        let text = report.to_string();
        assert!(text.contains("(found at"), "{text}");
        assert!(
            text.contains("v0.8.1-rc.1  (NOT FOUND on any source)"),
            "{text}"
        );
        assert!(text.contains("alice  preview"), "{text}");
    }

    #[test]
    fn show_says_who_is_not_listed_and_where_there_is_no_file() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        let other = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: Some("bob"),
        };

        let report = show_with(&other, &env_for(&setup)).expect("shows");
        assert_eq!(
            report.resolution.as_deref(),
            Some("bob: group 'default' (not listed) -> v0.8.0")
        );

        let nowhere = ShowOptions {
            root: Some(&setup.studio),
            team: None,
            user: None,
        };
        assert!(show_with(&nowhere, &env_for(&setup)).is_err());
    }

    #[test]
    fn show_may_read_the_runtime_variable() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        let variables: HashMap<String, String> = HashMap::from([
            ("STUDIO".to_string(), setup.studio.display().to_string()),
            (
                "ENVOY_CONFIG_PROD".to_string(),
                setup.root.display().to_string(),
            ),
        ]);
        let wanted = ShowOptions {
            root: None,
            team: None,
            user: None,
        };

        let report = show_with(&wanted, &|name| variables.get(name).cloned()).expect("shows");

        assert_eq!(report.live, setup.root.join("redirect.yaml"));
    }
}
