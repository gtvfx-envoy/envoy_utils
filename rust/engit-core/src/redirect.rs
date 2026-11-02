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
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use envoy_core::config::expand::expand_source;
use envoy_core::config::locate::{CONFIG_ROOT_VAR, REDIRECT_FILE_NAME};
use envoy_core::config::redirect::{normalize_user, Redirect, Severity};
use envoy_core::config::ConfigError;

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

/// What a history entry is called for a file whose publisher is not known.
const UNKNOWN_PUBLISHER: &str = "unknown";

/// A redirect file is a few lines. Anything bigger is the wrong file.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The empty file publishers take turns on, kept beside the redirect file.
const LOCK_FILE: &str = ".redirect.lock";

/// How long a publish waits for another one into the same folder to finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);

type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// An operating system whose launchers read the redirect file.
///
/// What counts as an Envoy version depends on it: the launcher on each system
/// looks for its own executable in a version's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `bin/envoy.exe`.
    Windows,
    /// `bin/envoy`.
    Linux,
    /// `bin/envoy`.
    MacOs,
}

impl Platform {
    /// Every system the launcher runs on.
    pub const ALL: [Platform; 3] = [Platform::Windows, Platform::Linux, Platform::MacOs];

    /// The system this program runs on, which is what a launcher started from
    /// the same machine would check.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }

    /// What the launcher looks for in a version's folder on this system.
    pub fn executable(self) -> &'static str {
        match self {
            Self::Windows => "bin/envoy.exe",
            Self::Linux | Self::MacOs => "bin/envoy",
        }
    }
}

/// The executables to look for, without repeats: this system's when none is named.
fn executables_for(platforms: &[Platform]) -> Vec<&'static str> {
    let host = [Platform::host()];
    let platforms = if platforms.is_empty() {
        &host[..]
    } else {
        platforms
    };

    let mut executables = Vec::new();
    for platform in platforms {
        let executable = platform.executable();
        if !executables.contains(&executable) {
            executables.push(executable);
        }
    }
    executables
}

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
    /// The systems whose launchers must find each version, or this system's
    /// when empty.
    pub platforms: &'a [Platform],
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
    /// The folder that holds it, for each system asked about (once when they
    /// share a folder).
    Found(Vec<PathBuf>),
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
    /// The file that was live before, kept in the history because nothing
    /// there had it yet (a file put there by hand).
    pub archived: Option<PathBuf>,
    /// Every group and the version it is pinned to.
    pub pins: Vec<PinnedVersion>,
    /// What the person publishing should know that is not an error.
    pub notes: Vec<String>,
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
        if let Some(archived) = &self.archived {
            let (label, tense) = if self.outcome == Outcome::WouldPublish {
                (
                    "To keep:",
                    "the file that is live now, which the history does not have",
                )
            } else {
                (
                    "Kept:",
                    "the file that was live, which the history did not have",
                )
            };
            writeln!(formatter, "  {label:<10}{} ({tense})", archived.display())?;
        }
        writeln!(formatter, "  Versions:")?;
        write_pins(formatter, &self.pins)?;
        for note in &self.notes {
            writeln!(formatter, "  Note: {note}")?;
        }
        Ok(())
    }
}

fn write_pins(formatter: &mut fmt::Formatter<'_>, pins: &[PinnedVersion]) -> fmt::Result {
    let width = pins.iter().map(|pin| pin.group.len()).max().unwrap_or(0);
    for pin in pins {
        write!(formatter, "    {:<width$}  {}", pin.group, pin.version)?;
        match &pin.availability {
            Availability::Found(folders) => {
                let folders: Vec<String> = folders
                    .iter()
                    .map(|folder| folder.display().to_string())
                    .collect();
                writeln!(formatter, "  (found at {})", folders.join(" and "))?
            }
            Availability::Missing(tried) => {
                writeln!(formatter, "  (NOT FOUND on any source)")?;
                for reason in tried {
                    writeln!(formatter, "        {reason}")?;
                }
            }
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
/// Otherwise, once it is this publisher's turn to publish into the folder, the
/// file that is live now is kept in the history if nothing there has it, a copy
/// of the new file is kept, and then the live file is replaced in one step: a
/// launcher reading it sees the old file or the new one, never half of either.
/// Every file that went live is in the history.
pub fn publish_redirect(options: &PublishOptions<'_>) -> Result<PublishReport> {
    let stamp = Utc::now().format(TIMESTAMP_FORMAT).to_string();
    publish_with(options, &|name| env::var(name).ok(), &stamp)
}

fn publish_with(options: &PublishOptions<'_>, env: &Env, stamp: &str) -> Result<PublishReport> {
    publish_waiting(options, env, stamp, LOCK_WAIT)
}

fn publish_waiting(
    options: &PublishOptions<'_>,
    env: &Env,
    stamp: &str,
    lock_wait: Duration,
) -> Result<PublishReport> {
    let root = publish_root(options.output, env)?;
    require_folder(&root)?;
    let directory = redirect_directory(&root, options.team)?;
    let live = directory.join(REDIRECT_FILE_NAME);

    let (text, bytes) = read_source(options.source)?;
    let redirect = Redirect::parse(&text).map_err(|error| parse_error(options.source, error))?;
    check_problems(options.source, &redirect)?;

    let pins = pinned_versions(&redirect, options.check_sources, options.platforms, env);
    if options.check_sources {
        check_found(&pins)?;
    }

    if is_same_file(options.source, &live) {
        return Err(EngitError::Redirect(format!(
            "{} is the live file itself. Publish a copy of it.",
            live.display()
        )));
    }

    let history_dir = directory.join(HISTORY_DIR);
    let history = history_dir.join(history_name(stamp, &publisher(env)));
    let mut report = PublishReport {
        outcome: Outcome::Unchanged,
        live: live.clone(),
        history: None,
        archived: None,
        pins,
        notes: Vec::new(),
    };

    let current = read_live(&live)?;
    if current.as_deref() == Some(bytes.as_slice()) {
        return Ok(report);
    }
    if options.dry_run {
        if history.exists() {
            return Err(history_taken(&history));
        }
        if let Some(current) = &current {
            report.archived = previous_entry(&history_dir, &live, current)?;
        }
        report.outcome = Outcome::WouldPublish;
        report.history = Some(history);
        return Ok(report);
    }

    create_folder(&directory)?;
    create_folder(&history_dir)?;
    let turn = PublishLock::acquire(&directory.join(LOCK_FILE), lock_wait)?;
    report.notes.extend(turn.note.clone());

    // It is this publisher's turn now, so look again: the file may have changed
    // while another publish was running.
    let current = read_live(&live)?;
    if current.as_deref() == Some(bytes.as_slice()) {
        return Ok(report);
    }
    if let Some(current) = &current {
        if let Some(path) = previous_entry(&history_dir, &live, current)? {
            write_history(&path, current)?;
            report.archived = Some(path);
        }
    }
    put_in_place(&history, &bytes, || replace_live(&directory, &live, &bytes))?;
    #[cfg(test)]
    tests::another_publisher_acts(&live);
    confirm_written(&live, &bytes)?;

    report.outcome = Outcome::Published;
    report.history = Some(history);
    Ok(report)
}

/// The live file, or `None` when there is none yet.
fn read_live(live: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(live) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EngitError::io(live, source)),
    }
}

/// A mistyped root must not become a new folder that no launcher reads.
fn require_folder(root: &Path) -> Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    Err(EngitError::Redirect(format!(
        "{} is not a folder. The config root must already exist: check the path.",
        root.display()
    )))
}

fn parse_error(file: &Path, error: ConfigError) -> EngitError {
    match error {
        ConfigError::NewerSchema { found, supported } => EngitError::Redirect(format!(
            "{} is written for schema {found}, but this engit understands up to schema \
{supported}. Update engit.",
            file.display()
        )),
        other => EngitError::Redirect(format!("{}: {other}", file.display())),
    }
}

/// Keep the history copy, then replace the live file.
///
/// A copy of a file that never went live must not stay in the history, so it is
/// removed when the replace fails.
fn put_in_place(
    history: &Path,
    bytes: &[u8],
    replace_live: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write_history(history, bytes)?;
    if let Err(error) = replace_live() {
        let _ = fs::remove_file(history);
        return Err(error);
    }
    Ok(())
}

fn confirm_written(live: &Path, bytes: &[u8]) -> Result<()> {
    match fs::read(live) {
        Ok(found) if found == bytes => Ok(()),
        Ok(_) => Err(EngitError::Redirect(format!(
            "{} does not have the content that was just published. Is someone else publishing?",
            live.display()
        ))),
        Err(source) => Err(EngitError::io(live, source)),
    }
}

/// The turn to publish into a folder, held until it is dropped.
///
/// It is an operating system lock on an empty file, so it is let go when the
/// program ends however it ends, and a crashed publish never leaves a lock for
/// someone to clear.
struct PublishLock {
    file: File,
    /// Set when the folder cannot lock files, so there was nothing to wait for.
    note: Option<String>,
}

impl PublishLock {
    fn acquire(path: &Path, wait: Duration) -> Result<Self> {
        let file = open_lock_file(path)?;
        let give_up = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file, note: None }),
                Err(TryLockError::WouldBlock) if Instant::now() < give_up => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(TryLockError::WouldBlock) => {
                    let folder = path.parent().unwrap_or(path);
                    return Err(EngitError::Redirect(format!(
                        "Another publish into {} is still running after {:.1} seconds. Try again \
in a moment.",
                        folder.display(),
                        wait.as_secs_f32()
                    )));
                }
                // Some network locations cannot lock files. Publishing still works there.
                Err(TryLockError::Error(error)) => {
                    let folder = path.parent().unwrap_or(path);
                    return Ok(Self {
                        file,
                        note: Some(format!(
                            "{} cannot lock files ({error}), so nothing stopped another publish \
into it from running at the same time.",
                            folder.display()
                        )),
                    });
                }
            }
        }
    }
}

impl Drop for PublishLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path);
    match opened {
        Ok(file) => {
            // Whoever publishes next, whatever their umask, must be able to open it.
            let _ = make_open_to_all(&file);
            Ok(file)
        }
        // A read-only handle holds the lock just as well.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|source| EngitError::io(path, source)),
        Err(source) => Err(EngitError::io(path, source)),
    }
}

/// Create `path` when it is not there, open to the people who can use its parent.
fn create_folder(path: &Path) -> Result<()> {
    // A drive or a share root cannot be created again, and Windows says "access
    // is denied" for it rather than "already exists".
    if path.is_dir() {
        return Ok(());
    }
    match fs::create_dir(path) {
        Ok(()) => {
            match_parent_access(path);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(source) => Err(EngitError::io(path, source)),
    }
}

/// Who can use a new folder follows the publisher's umask unless it is set, and
/// another administrator could then not publish into it, or a launcher not read
/// it.
///
/// Everyone can read and enter it, as in a folder made under the usual umask.
/// From the folder it is in, it takes the group's access and the group bit, so
/// that the administrators who can publish there can publish here. It takes
/// nothing of what the parent gives everyone else, and not its sticky bit: a
/// world-writable root must not make a world-writable folder.
#[cfg(unix)]
fn match_parent_access(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Some(parent) = path.parent() else { return };
    if let Ok(metadata) = fs::metadata(parent) {
        let mode = 0o755 | (metadata.permissions().mode() & 0o2070);
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
}

#[cfg(not(unix))]
fn match_parent_access(_path: &Path) {}

/// The launcher of every user reads the file, whoever published it and
/// whatever their umask is.
///
/// Callers take no notice of a failure: a filesystem that keeps no modes (vfat,
/// exFAT, some network mounts) refuses, and the file there is as readable as the
/// filesystem makes anything.
#[cfg(unix)]
fn make_readable_by_all(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    #[cfg(test)]
    if tests::modes_are_refused() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    file.set_permissions(fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn make_readable_by_all(_file: &File) -> io::Result<()> {
    Ok(())
}

/// The lock file is opened by whoever publishes next.
#[cfg(unix)]
fn make_open_to_all(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o666))
}

#[cfg(not(unix))]
fn make_open_to_all(_file: &File) -> io::Result<()> {
    Ok(())
}

fn history_taken(history: &Path) -> EngitError {
    EngitError::Redirect(format!(
        "{} already exists. Someone published as the same user in the same second: try again.",
        history.display()
    ))
}

/// Create a history entry, which must not be there yet: an entry is never replaced.
fn write_history(history: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(history)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(history_taken(history));
        }
        Err(source) => return Err(EngitError::io(history, source)),
    };

    let written = file.write_all(bytes).and_then(|()| {
        let _ = make_readable_by_all(&file);
        file.sync_all()
    });
    drop(file);
    if let Err(source) = written {
        let _ = fs::remove_file(history);
        return Err(EngitError::io(history, source));
    }
    Ok(())
}

/// Where the file that is live now has to be kept, so that the history has
/// every file that went live, or `None` when an entry already has it.
///
/// Nobody knows who published a file that was put there by hand, so it is named
/// for the time it was last changed and for an unknown publisher.
fn previous_entry(history_dir: &Path, live: &Path, current: &[u8]) -> Result<Option<PathBuf>> {
    if history_holds(history_dir, current) {
        return Ok(None);
    }

    let now = SystemTime::now();
    let changed = fs::metadata(live)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(now);
    // A file dated in the future, from a clock that was ahead, must not be filed
    // under a time that has not come: it would look newer than what replaces it.
    let changed = DateTime::<Utc>::from(changed.min(now));
    for earlier in 0..60 {
        let stamp = (changed - chrono::Duration::seconds(earlier))
            .format(TIMESTAMP_FORMAT)
            .to_string();
        let path = history_dir.join(history_name(&stamp, UNKNOWN_PUBLISHER));
        if !path.exists() {
            return Ok(Some(path));
        }
    }
    Err(EngitError::Redirect(format!(
        "{} has no free name for the file that is live now.",
        history_dir.display()
    )))
}

/// The history entries in `history_dir`, newest first.
///
/// A name starts with the time the entry was published, in a form that sorts
/// the way time does.
fn entries_newest_first(history_dir: &Path) -> Vec<PathBuf> {
    let Ok(listing) = fs::read_dir(history_dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = listing
        .flatten()
        .filter(|item| parse_history_name(&item.file_name().to_string_lossy()).is_some())
        .map(|item| item.path())
        .collect();
    entries.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    entries
}

/// `true` when a history entry has exactly these bytes.
///
/// The file that is live is nearly always the newest entry, so the search ends
/// at the first read. It matters because it runs while holding the turn to
/// publish, and a history grows for years.
fn history_holds(history_dir: &Path, bytes: &[u8]) -> bool {
    entries_newest_first(history_dir).iter().any(|path| {
        fs::metadata(path).is_ok_and(|metadata| metadata.len() == bytes.len() as u64)
            && fs::read(path).is_ok_and(|found| found == bytes)
    })
}

/// Write `bytes` beside the live file and move them over it.
fn replace_live(directory: &Path, live: &Path, bytes: &[u8]) -> Result<()> {
    let unique = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let staged = directory.join(format!(
        ".{REDIRECT_FILE_NAME}.{}-{unique}.tmp",
        std::process::id()
    ));

    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            let _ = make_readable_by_all(&file);
            file.sync_all()
        });
    if let Err(source) = written {
        let _ = fs::remove_file(&staged);
        return Err(EngitError::io(&staged, source));
    }

    rename_with_retry(&staged, live).map_err(|source| {
        let _ = fs::remove_file(&staged);
        EngitError::io(live, source)
    })
}

/// Rename `from` over `to`, retrying for a moment while another program has
/// `to` open.
///
/// The launcher reads the file each time it starts, so a publish can land while
/// it is open. A program that opens a file without allowing it to be deleted
/// makes the rename fail, and Windows reports that as a sharing violation or as
/// "access is denied" depending on how it was opened. The wait is short because
/// the same "access is denied" also means the publisher may not change the file.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 8;

    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 < ATTEMPTS && is_busy(&error) => {
                attempt += 1;
                thread::sleep(Duration::from_millis(50 * u64::from(attempt)));
            }
            Err(error) => return Err(error),
        }
    }
}

/// `true` when another program may have the file open for a moment (Windows).
fn is_busy(error: &io::Error) -> bool {
    // ACCESS_DENIED, SHARING_VIOLATION, LOCK_VIOLATION
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
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

fn pinned_versions(
    redirect: &Redirect,
    check_sources: bool,
    platforms: &[Platform],
    env: &Env,
) -> Vec<PinnedVersion> {
    let executables = executables_for(platforms);
    redirect
        .versions
        .iter()
        .map(|(group, version)| PinnedVersion {
            group: group.clone(),
            version: version.clone(),
            availability: if check_sources {
                find_app(version, &redirect.sources, &executables, env)
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

/// Look for a version the way the launcher does, for each executable asked
/// about: the first source whose folder has it as a file.
fn find_app(version: &str, sources: &[String], executables: &[&str], env: &Env) -> Availability {
    let mut folders = Vec::new();
    let mut tried = Vec::new();
    for executable in executables {
        match find_executable(version, sources, executable, env) {
            Ok(folder) => {
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            Err(reasons) => {
                for reason in reasons {
                    if !tried.contains(&reason) {
                        tried.push(reason);
                    }
                }
            }
        }
    }

    if tried.is_empty() {
        Availability::Found(folders)
    } else {
        Availability::Missing(tried)
    }
}

fn find_executable(
    version: &str,
    sources: &[String],
    executable: &str,
    env: &Env,
) -> std::result::Result<PathBuf, Vec<String>> {
    let mut tried = Vec::new();
    for template in sources {
        match expand_source(template, version, env) {
            Ok(folder) => {
                if folder.join(executable).is_file() {
                    return Ok(folder);
                }
                tried.push(format!(
                    "{}: there is no {executable} there",
                    folder.display()
                ));
            }
            Err(error) => tried.push(error.to_string()),
        }
    }
    Err(tried)
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
            "'{team}' cannot be a team name: use letters, digits, '.', '_' and '-', and do not \
end it with a '.'."
        ))),
    }
}

/// The launcher's rule for a team name, which becomes a folder name.
///
/// A name may not end with a dot either, which is stricter than the launcher:
/// Windows drops trailing dots from a folder name, so such a name would publish
/// into another folder than the one it names.
fn is_safe_team(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.ends_with('.')
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
        UNKNOWN_PUBLISHER.to_string()
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
    /// The systems whose launchers must find each version, or this system's
    /// when empty.
    pub platforms: &'a [Platform],
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
    require_folder(&root)?;
    let directory = redirect_directory(&root, options.team)?;
    require_team_folder(&directory, options.team)?;
    let live = directory.join(REDIRECT_FILE_NAME);
    if !live.is_file() {
        return Err(EngitError::Redirect(format!(
            "No redirect file is published at {}.",
            live.display()
        )));
    }
    let (text, _) = read_source(&live)?;
    let redirect = Redirect::parse(&text).map_err(|error| parse_error(&live, error))?;

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
        pins: pinned_versions(&redirect, true, options.platforms, env),
        users: listed_users(&text, &redirect),
        resolution,
        live,
    })
}

/// A team's folder is made by the team's first publish, so a name with no
/// folder is mistyped or has nothing published.
fn require_team_folder(directory: &Path, team: Option<&str>) -> Result<()> {
    match team {
        Some(team) if !directory.is_dir() => Err(EngitError::Redirect(format!(
            "{} does not exist, so nothing has been published for team '{team}'. Check the name.",
            directory.display()
        ))),
        _ => Ok(()),
    }
}

/// The users a file lists, with the group the launcher gives each.
///
/// The parsed file keeps its users to itself. The names are read from the text
/// and the group is asked of the parsed file, so that what is shown is what the
/// launcher does even when a user is listed twice.
fn listed_users(text: &str, redirect: &Redirect) -> BTreeMap<String, String> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    let mut users = BTreeMap::new();
    if let Some(listed) = value.get("users").and_then(serde_yaml::Value::as_mapping) {
        for (user, _) in listed {
            let Some(user) = user.as_str() else { continue };
            let user = normalize_user(user);
            let group = match redirect.resolve(&user) {
                Ok(found) => found.group,
                Err(error) => format!("({error})"),
            };
            users.insert(user, group);
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
    let root = read_root(root, env)?;
    require_folder(&root)?;
    let directory = redirect_directory(&root, team)?;
    require_team_folder(&directory, team)?;
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
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use chrono::Utc;
    use tempfile::{tempdir, TempDir};

    use super::{
        confirm_written, create_folder, entries_newest_first, history_with, parse_history_name,
        publish_waiting, publish_with, put_in_place, show_with, Availability, Outcome, Platform,
        PublishLock, PublishOptions, ShowOptions, CONFIG_PUBLISH_ROOT_VAR, LOCK_FILE,
        TIMESTAMP_FORMAT,
    };
    use crate::error::EngitError;

    /// What a test does to the live file after it was replaced.
    type Act = Box<dyn Fn(&Path)>;

    /// What a test makes go wrong, on the thread it runs on.
    #[derive(Default)]
    struct Faults {
        /// Done to the live file just after it was replaced, as another publisher could.
        after_replace: Option<Act>,
        /// The filesystem refuses to change modes.
        #[cfg_attr(not(unix), allow(dead_code))]
        refuse_modes: bool,
    }

    thread_local! {
        static FAULTS: RefCell<Faults> = RefCell::new(Faults::default());
    }

    /// Takes the faults away when it is dropped, even if the test panics.
    struct FaultGuard;

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            FAULTS.with(|faults| *faults.borrow_mut() = Faults::default());
        }
    }

    pub(super) fn another_publisher_acts(live: &Path) {
        FAULTS.with(|faults| {
            if let Some(act) = &faults.borrow().after_replace {
                act(live);
            }
        });
    }

    #[cfg(unix)]
    pub(super) fn modes_are_refused() -> bool {
        FAULTS.with(|faults| faults.borrow().refuse_modes)
    }

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
        make_app_for(studio, version, &[Platform::host().executable()]);
    }

    /// An app folder that has exactly these executables, as an archive for each system adds.
    fn make_app_for(studio: &Path, version: &str, executables: &[&str]) {
        let folder = studio.join("envoy").join(version);
        let _ = fs::remove_dir_all(&folder);
        for executable in executables {
            let path = folder.join(executable);
            fs::create_dir_all(path.parent().expect("an executable has a folder"))
                .expect("bin is created");
            fs::write(&path, "").expect("executable is written");
        }
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
            platforms: &[],
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
    fn only_the_files_and_the_lock_are_left_behind() {
        let setup = setup();

        publish(&setup, &options(&setup), STAMP).expect("publishes");

        assert_eq!(
            files_under(&setup.root),
            [
                LOCK_FILE,
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
        assert_eq!(files_under(&setup.root).len(), 3);
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
        assert_eq!(files_under(&setup.root).len(), 4);
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

        assert!(text.contains("schema 99"), "{text}");
        assert!(text.contains("Update engit"), "{text}");
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
        let executable = Platform::host().executable();
        fs::remove_file(setup.studio.join("envoy/v0.8.0").join(executable)).expect("removed");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains(&format!("there is no {executable} there")),
            "{text}"
        );
    }

    #[test]
    fn a_directory_with_the_name_of_the_executable_is_not_a_version() {
        let setup = setup();
        let executable = setup
            .studio
            .join("envoy/v0.8.0")
            .join(Platform::host().executable());
        fs::remove_file(&executable).expect("removed");
        fs::create_dir(&executable).expect("a directory takes its place");

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains("group 'default' is pinned to v0.8.0"),
            "{text}"
        );
    }

    #[test]
    fn each_system_is_held_to_the_executable_its_launcher_looks_for() {
        let setup = setup();
        let versions = ["v0.8.0", "v0.8.1-rc.1"];
        let stock = |executables: &[&str]| {
            for version in versions {
                make_app_for(&setup.studio, version, executables);
            }
        };
        let publishes_for = |platforms: &[Platform]| {
            let mut wanted = options(&setup);
            wanted.platforms = platforms;
            wanted.dry_run = true;
            publish(&setup, &wanted, STAMP)
        };

        // Only the Windows executable, as a Windows-only archive unpacks.
        stock(&["bin/envoy.exe"]);
        assert!(publishes_for(&[Platform::Windows]).is_ok());
        for platform in [Platform::Linux, Platform::MacOs] {
            let text = message(publishes_for(&[platform]));
            assert!(text.contains("there is no bin/envoy there"), "{text}");
        }
        let text = message(publishes_for(&Platform::ALL));
        assert!(text.contains("there is no bin/envoy there"), "{text}");
        assert!(!text.contains("there is no bin/envoy.exe"), "{text}");

        // Only the plain executable, as a Linux-only archive unpacks.
        stock(&["bin/envoy"]);
        assert!(publishes_for(&[Platform::Linux, Platform::MacOs]).is_ok());
        let text = message(publishes_for(&[Platform::Windows]));
        assert!(text.contains("there is no bin/envoy.exe there"), "{text}");

        // Both, which is what a studio for every system needs.
        stock(&["bin/envoy.exe", "bin/envoy"]);
        let report = publishes_for(&Platform::ALL).expect("every system finds it");
        assert!(report.pins.iter().all(|pin| matches!(
            &pin.availability,
            Availability::Found(folders) if folders.len() == 1
        )));
    }

    #[test]
    fn a_publish_that_names_no_system_is_held_to_this_one() {
        let setup = setup();
        let other = if cfg!(windows) {
            "bin/envoy"
        } else {
            "bin/envoy.exe"
        };
        for version in ["v0.8.0", "v0.8.1-rc.1"] {
            make_app_for(&setup.studio, version, &[other]);
        }

        let text = message(publish(&setup, &options(&setup), STAMP));

        assert!(
            text.contains(&format!(
                "there is no {} there",
                Platform::host().executable()
            )),
            "{text}"
        );
    }

    #[test]
    fn a_source_that_cannot_be_expanded_is_passed_over_for_the_next_one() {
        let setup = setup();
        fs::write(
            &setup.source,
            FILE.replace(
                "  - ${STUDIO}/envoy/{version}",
                "  - ${NOT_SET}/envoy/{version}\n  - ${STUDIO}/envoy/{version}",
            ),
        )
        .expect("written");

        let report = publish(&setup, &options(&setup), STAMP).expect("the second source has it");

        assert!(report
            .pins
            .iter()
            .all(|pin| matches!(pin.availability, Availability::Found(_))));
    }

    #[test]
    fn the_first_source_that_has_a_version_is_the_one_that_is_reported() {
        let setup = setup();
        let second = setup.studio.with_file_name("second");
        for version in ["v0.8.0", "v0.8.1-rc.1"] {
            make_app(&second, version);
        }
        let variables: HashMap<String, String> = HashMap::from([
            ("EMPTY".to_string(), setup.root.display().to_string()),
            ("STUDIO".to_string(), second.display().to_string()),
        ]);
        fs::write(
            &setup.source,
            FILE.replace(
                "  - ${STUDIO}/envoy/{version}",
                "  - ${EMPTY}/envoy/{version}\n  - ${STUDIO}/envoy/{version}",
            ),
        )
        .expect("written");

        let report = publish_with(
            &options(&setup),
            &|name| variables.get(name).cloned(),
            STAMP,
        )
        .expect("the second source has it");

        assert!(report.pins.iter().all(|pin| pin.availability
            == Availability::Found(vec![second.join("envoy").join(&pin.version)])));
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
                "rigging/.redirect.lock",
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
        let first = setup
            .root
            .join("history")
            .join("redirect-2026-11-02-171000-alice.yaml");
        assert_eq!(fs::read_to_string(first).expect("first reads"), FILE);
    }

    #[test]
    fn a_live_path_that_cannot_be_read_changes_nothing() {
        let setup = setup();
        // A folder where the live file belongs.
        fs::create_dir_all(setup.root.join("redirect.yaml")).expect("blocker is created");

        let error = publish(&setup, &options(&setup), STAMP).expect_err("it cannot be read");

        assert!(matches!(error, EngitError::Io { .. }), "{error}");
        assert!(files_under(&setup.root).is_empty());
        assert!(!setup.root.join("history").exists());
    }

    #[test]
    fn a_history_copy_of_a_file_that_never_went_live_is_removed() {
        let setup = setup();
        let history = setup.root.join("history");
        fs::create_dir_all(&history).expect("history is created");
        let entry = history.join("redirect-2026-11-02-171000-alice.yaml");

        let error = put_in_place(&entry, b"new", || {
            Err(EngitError::Redirect("the replace failed".to_string()))
        })
        .expect_err("the replace fails");

        assert!(error.to_string().contains("the replace failed"), "{error}");
        assert!(!entry.exists(), "the copy never went live, so it must go");

        put_in_place(&entry, b"new", || Ok(())).expect("a replace that works keeps it");
        assert_eq!(fs::read(&entry).expect("entry reads"), b"new");
    }

    #[test]
    fn what_was_published_is_checked_by_reading_it_back() {
        let setup = setup();
        let live = setup.root.join("redirect.yaml");
        fs::write(&live, "somebody else's file").expect("live is written");

        let error = confirm_written(&live, b"what was published").expect_err("they differ");

        assert!(
            error.to_string().contains("does not have the content"),
            "{error}"
        );
        assert!(confirm_written(&live, b"somebody else's file").is_ok());
        assert!(confirm_written(&setup.root.join("missing.yaml"), b"x").is_err());
    }

    #[test]
    fn a_publish_waits_for_another_one_to_finish() {
        let setup = setup();
        let held = PublishLock::acquire(&setup.root.join(LOCK_FILE), Duration::from_secs(1))
            .expect("the first publisher has its turn");
        let released = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&released);
        let finish = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::SeqCst);
            drop(held);
        });

        let report = publish_waiting(
            &options(&setup),
            &env_for(&setup),
            STAMP,
            Duration::from_secs(10),
        )
        .expect("it goes ahead once the other has finished");

        assert!(
            released.load(Ordering::SeqCst),
            "the publish went ahead while the other still had its turn"
        );
        finish.join().expect("the other publisher finishes");
        assert_eq!(report.outcome, Outcome::Published);
    }

    #[test]
    fn a_file_put_there_while_waiting_for_the_turn_is_kept() {
        let setup = setup();
        let held = PublishLock::acquire(&setup.root.join(LOCK_FILE), Duration::from_secs(1))
            .expect("the first publisher has its turn");
        let live = setup.root.join("redirect.yaml");
        let by_hand = "schema: 1\nsources: ['${STUDIO}/envoy/{version}']\nversions: {default: v0.8.0}\n# by hand\n";
        let other = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            // Somebody changes the live file while this publish is waiting for its turn.
            fs::write(&live, by_hand).expect("a file is put there by hand");
            drop(held);
        });

        publish_waiting(
            &options(&setup),
            &env_for(&setup),
            STAMP,
            Duration::from_secs(10),
        )
        .expect("it goes ahead once the other has finished");

        other.join().expect("the other finishes");
        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");
        let kept: Vec<String> = entries
            .iter()
            .map(|entry| fs::read_to_string(&entry.path).expect("entry reads"))
            .collect();
        assert!(kept.iter().any(|text| text == by_hand), "{kept:?}");
    }

    #[test]
    fn a_publish_whose_file_was_changed_underneath_it_says_so() {
        let setup = setup();
        let _faults = FaultGuard;
        FAULTS.with(|faults| {
            faults.borrow_mut().after_replace = Some(Box::new(|live| {
                fs::write(live, "somebody else's file").expect("another publisher writes");
            }));
        });

        let error =
            publish(&setup, &options(&setup), STAMP).expect_err("the read-back finds another file");

        assert!(
            error.to_string().contains("does not have the content"),
            "{error}"
        );
    }

    #[test]
    fn a_publish_that_never_gets_its_turn_says_so_and_changes_nothing() {
        let setup = setup();
        let _held = PublishLock::acquire(&setup.root.join(LOCK_FILE), Duration::from_secs(1))
            .expect("the first publisher has its turn");

        let error = publish_waiting(
            &options(&setup),
            &env_for(&setup),
            STAMP,
            Duration::from_millis(250),
        )
        .expect_err("it gives up");

        assert!(error.to_string().contains("is still running"), "{error}");
        assert_eq!(files_under(&setup.root), [LOCK_FILE]);
    }

    #[test]
    fn a_turn_is_free_again_once_it_is_let_go() {
        let setup = setup();
        let path = setup.root.join(LOCK_FILE);

        drop(PublishLock::acquire(&path, Duration::from_secs(1)).expect("the first turn"));

        PublishLock::acquire(&path, Duration::from_millis(100)).expect("free again");
    }

    #[test]
    fn a_dry_run_says_which_file_it_would_keep_and_writes_nothing() {
        let setup = setup();
        fs::write(setup.root.join("redirect.yaml"), "put there by hand\n").expect("by hand");
        let mut dry = options(&setup);
        dry.dry_run = true;

        let report = publish(&setup, &dry, STAMP).expect("checks");

        let kept = report.archived.clone().expect("the old file would be kept");
        let name = kept
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a name");
        assert!(
            name.starts_with("redirect-") && name.ends_with("-unknown.yaml"),
            "{name}"
        );
        assert!(report.to_string().contains("To keep:"), "{report}");
        assert_eq!(files_under(&setup.root), ["redirect.yaml"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_folder_is_open_to_the_same_people_as_the_one_it_is_in() {
        use std::os::unix::fs::PermissionsExt;

        let setup = setup();
        fs::set_permissions(&setup.root, fs::Permissions::from_mode(0o775)).expect("mode is set");
        let mut team = options(&setup);
        team.team = Some("rigging");

        publish(&setup, &team, STAMP).expect("publishes");

        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&setup.root.join("rigging")), 0o775);
        assert_eq!(mode(&setup.root.join("rigging/history")), 0o775);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_folder_takes_nothing_from_what_its_parent_gives_everyone() {
        use std::os::unix::fs::PermissionsExt;

        let setup = setup();
        // A world-writable folder with the sticky bit, like a shared temp folder.
        fs::set_permissions(&setup.root, fs::Permissions::from_mode(0o1777)).expect("mode is set");
        let mut team = options(&setup);
        team.team = Some("rigging");

        publish(&setup, &team, STAMP).expect("publishes");

        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode();
        for folder in ["rigging", "rigging/history"] {
            let mode = mode(&setup.root.join(folder));
            assert_eq!(mode & 0o1002, 0, "{folder} is {mode:o}");
            assert_eq!(mode & 0o755, 0o755, "{folder} is {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_filesystem_that_refuses_to_change_modes_can_still_be_published_to() {
        let setup = setup();
        let _faults = FaultGuard;
        FAULTS.with(|faults| faults.borrow_mut().refuse_modes = true);

        let report = publish(&setup, &options(&setup), STAMP).expect("publishes anyway");

        assert_eq!(report.outcome, Outcome::Published);
        assert_eq!(fs::read_to_string(&report.live).expect("live reads"), FILE);
        assert!(report.history.is_some_and(|history| history.is_file()));
    }

    #[test]
    fn a_folder_that_is_there_is_left_alone() {
        let setup = setup();
        fs::write(setup.root.join("a.txt"), "x").expect("a file is written");

        create_folder(&setup.root).expect("it is a folder already");

        assert_eq!(files_under(&setup.root), ["a.txt"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_a_folder_that_is_already_there() {
        // Windows refuses to create a drive root with "access is denied", not "already exists".
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());

        create_folder(Path::new(&format!("{drive}\\"))).expect("a drive root is a folder");
    }

    #[test]
    fn history_entries_are_searched_newest_first() {
        let setup = setup();
        let history = setup.root.join("history");
        fs::create_dir_all(&history).expect("history is created");
        for name in [
            "redirect-2026-11-02-171000-alice.yaml",
            "redirect-2026-11-02-171200-bob.yaml",
            "redirect-2026-10-30-094512-unknown.yaml",
            "redirect-bad.yaml",
            "notes.txt",
        ] {
            fs::write(history.join(name), "x").expect("a file is written");
        }

        let names: Vec<String> = entries_newest_first(&history)
            .iter()
            .map(|path| {
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(
            names,
            [
                "redirect-2026-11-02-171200-bob.yaml",
                "redirect-2026-11-02-171000-alice.yaml",
                "redirect-2026-10-30-094512-unknown.yaml",
            ]
        );
    }

    #[test]
    fn a_dry_run_refuses_a_history_name_that_is_taken_as_the_real_publish_would() {
        let setup = setup();
        let history = setup.root.join("history");
        fs::create_dir_all(&history).expect("history is created");
        fs::write(
            history.join("redirect-2026-11-02-171000-alice.yaml"),
            "taken",
        )
        .expect("the name is taken");
        let mut dry = options(&setup);
        dry.dry_run = true;

        let error = publish(&setup, &dry, STAMP).expect_err("the name is taken");

        assert!(error.to_string().contains("already exists"), "{error}");
    }

    /// Put a file there by hand, last changed at `changed`.
    fn put_by_hand(setup: &Setup, changed: SystemTime) {
        let live = setup.root.join("redirect.yaml");
        fs::write(&live, "put there by hand\n").expect("by hand");
        fs::File::options()
            .write(true)
            .open(&live)
            .and_then(|file| file.set_modified(changed))
            .expect("the time is set");
    }

    #[test]
    fn a_file_that_was_live_is_filed_under_the_time_it_was_last_changed() {
        let setup = setup();
        // 2023-11-14 22:13:20 UTC
        put_by_hand(
            &setup,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        );

        let report = publish(&setup, &options(&setup), STAMP).expect("publishes");

        let kept = report.archived.expect("the old file is kept");
        assert_eq!(
            kept.file_name().and_then(|name| name.to_str()),
            Some("redirect-2023-11-14-221320-unknown.yaml")
        );
    }

    #[test]
    fn a_file_dated_in_the_future_is_filed_under_the_time_it_is_now() {
        let setup = setup();
        put_by_hand(
            &setup,
            SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60),
        );

        let report = publish(&setup, &options(&setup), STAMP).expect("publishes");

        let kept = report.archived.expect("the old file is kept");
        let name = kept
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a name")
            .to_string();
        let stamp = &name["redirect-".len()..][..super::TIMESTAMP_LENGTH];
        let now = Utc::now().format(TIMESTAMP_FORMAT).to_string();
        assert!(stamp <= now.as_str(), "{stamp} has not come yet: {now}");
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_can_be_opened_by_whoever_publishes_next() {
        use std::os::unix::fs::PermissionsExt;

        let setup = setup();

        publish(&setup, &options(&setup), STAMP).expect("publishes");

        let mode = fs::metadata(setup.root.join(LOCK_FILE))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o666, 0o666, "{mode:o}");
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
        fs::remove_file(
            setup
                .studio
                .join("envoy/v0.8.1-rc.1")
                .join(Platform::host().executable()),
        )
        .expect("removed");
        let wanted = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: Some("CORP\\ALICE"),
            platforms: &[],
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
            platforms: &[],
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
            platforms: &[],
        };
        let text = show_with(&nowhere, &env_for(&setup))
            .expect_err("there is no file there")
            .to_string();
        assert!(text.contains("No redirect file is published at"), "{text}");
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
            platforms: &[],
        };

        let report = show_with(&wanted, &|name| variables.get(name).cloned()).expect("shows");

        assert_eq!(report.live, setup.root.join("redirect.yaml"));
    }

    #[test]
    fn two_publishers_with_one_name_in_one_second_cannot_both_win() {
        for round in 0..8 {
            let setup = Arc::new(setup());
            let barrier = Arc::new(Barrier::new(6));
            let attempts: Vec<_> = (0..6)
                .map(|number| {
                    let (setup, barrier) = (Arc::clone(&setup), Arc::clone(&barrier));
                    thread::spawn(move || {
                        let source = setup.root.with_file_name(format!("redirect-{number}.yaml"));
                        let text =
                            FILE.replace("alice: preview", &format!("user{number}: preview"));
                        fs::write(&source, text).expect("source is written");
                        let mut wanted = options(&setup);
                        wanted.source = &source;
                        barrier.wait();
                        publish(&setup, &wanted, STAMP).is_ok()
                    })
                })
                .collect();

            let won = attempts
                .into_iter()
                .map(|attempt| attempt.join().expect("the thread finishes"))
                .filter(|published| *published)
                .count();

            assert_eq!(won, 1, "round {round}: only one may take the history name");
            let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");
            assert_eq!(entries.len(), 1, "round {round}");
            assert!(
                entries[0].live,
                "round {round}: the entry is what went live"
            );
        }
    }

    #[test]
    fn the_file_that_was_live_before_is_kept_in_the_history() {
        let setup = setup();
        let before = "schema: 1\nsources: ['${STUDIO}/envoy/{version}']\nversions: {default: v0.8.0}\n# by hand\n";
        fs::write(setup.root.join("redirect.yaml"), before).expect("a file is put there by hand");

        publish(&setup, &options(&setup), STAMP).expect("publishes");

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");
        let kept: Vec<String> = entries
            .iter()
            .map(|entry| fs::read_to_string(&entry.path).expect("entry reads"))
            .collect();
        assert!(kept.iter().any(|text| text == before), "{kept:?}");
        assert!(kept.iter().any(|text| text == FILE), "{kept:?}");
        assert_eq!(entries.iter().filter(|entry| entry.live).count(), 1);
    }

    #[test]
    fn a_previous_file_that_is_in_the_history_already_is_not_copied_again() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");

        publish(&setup, &options(&setup), "2026-11-02-171100").expect("publishes");

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");
        assert_eq!(entries.len(), 2, "{entries:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_live_file_another_program_holds_open_is_replaced_once_it_lets_go() {
        use std::os::windows::fs::OpenOptionsExt;

        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");
        let live = setup.root.join("redirect.yaml");
        // Shared for reading and writing but not for deleting, the way an older program opens a file.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&live)
            .expect("the live file is held");
        let let_go = thread::spawn(move || {
            thread::sleep(Duration::from_millis(400));
            drop(held);
        });

        let outcome = publish(&setup, &options(&setup), "2026-11-02-171100");

        let_go.join().expect("the holder finishes");
        outcome.expect("the publish waits for the file to be let go");
        assert!(fs::read_to_string(&live)
            .expect("live reads")
            .contains("alice: default"));
    }

    #[cfg(windows)]
    #[test]
    fn a_live_file_that_stays_open_is_left_as_it_was() {
        use std::os::windows::fs::OpenOptionsExt;

        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        fs::write(
            &setup.source,
            FILE.replace("alice: preview", "alice: default"),
        )
        .expect("source is changed");
        let live = setup.root.join("redirect.yaml");
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&live)
            .expect("the live file is held");

        let error = publish(&setup, &options(&setup), "2026-11-02-171100")
            .expect_err("the file cannot be replaced while it is held");

        assert!(matches!(error, EngitError::Io { .. }), "{error}");
        assert_eq!(fs::read_to_string(&live).expect("live reads"), FILE);
        assert_eq!(
            files_under(&setup.root).len(),
            3,
            "no history entry or temporary file is left"
        );
    }

    #[test]
    fn show_lists_the_group_the_launcher_uses_when_a_user_is_listed_twice() {
        let setup = setup();
        fs::write(
            setup.root.join("redirect.yaml"),
            FILE.replace("alice: preview", "alice: default\n  ALICE: preview"),
        )
        .expect("a file is put there by hand");
        let wanted = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: Some("alice"),
            platforms: &[],
        };

        let report = show_with(&wanted, &env_for(&setup)).expect("shows");

        let listed = report.users.get("alice").expect("alice is listed").clone();
        let answer = report.resolution.expect("alice is asked about");
        assert!(
            answer.contains(&format!("group '{listed}'")),
            "the table says {listed}, the answer says {answer}"
        );
    }

    #[test]
    fn show_says_why_a_version_was_not_found() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        let wanted = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: None,
            platforms: &[],
        };

        let report = show_with(&wanted, &|_: &str| None).expect("shows");

        let text = report.to_string();
        assert!(text.contains("NOT FOUND on any source"), "{text}");
        assert!(text.contains("STUDIO is not set"), "{text}");
    }

    #[test]
    fn history_of_a_root_or_a_team_that_does_not_exist_is_an_error() {
        let setup = setup();
        let mistyped = setup.root.with_file_name("confg");

        let root = history_with(Some(&mistyped), None, &env_for(&setup))
            .expect_err("a root that is not there is not an empty history");
        let team = history_with(Some(&setup.root), Some("rigging"), &env_for(&setup))
            .expect_err("a team with no folder is not an empty history");

        assert!(root.to_string().contains("is not a folder"), "{root}");
        assert!(team.to_string().contains("rigging"), "{team}");
    }

    #[test]
    fn a_name_the_system_would_alter_is_not_a_team_name() {
        let setup = setup();
        for name in ["rigging.", "pre..", "a.", "a b", " a"] {
            let mut team = options(&setup);
            team.team = Some(name);

            let text = message(publish(&setup, &team, STAMP));

            assert!(text.contains("cannot be a team name"), "{name:?}: {text}");
        }
    }

    #[test]
    fn a_user_name_that_cannot_be_a_file_name_is_made_into_one() {
        let setup = setup();
        let variables: HashMap<String, String> = HashMap::from([
            ("STUDIO".to_string(), setup.studio.display().to_string()),
            ("USERNAME".to_string(), "CORP\\O'Brien <x>".to_string()),
        ]);

        let report = publish_with(
            &options(&setup),
            &|name| variables.get(name).cloned(),
            STAMP,
        )
        .expect("publishes");

        let history = report.history.expect("a history copy is kept");
        assert_eq!(
            history.file_name().and_then(|name| name.to_str()),
            Some("redirect-2026-11-02-171000-o_brien__x_.yaml")
        );
    }

    #[test]
    fn a_history_file_with_a_name_that_is_not_ascii_is_ignored() {
        let setup = setup();
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        // A character that straddles the byte where the stamp ends.
        fs::write(
            setup
                .root
                .join("history")
                .join("redirect-2026-11-02-17100é-alice.yaml"),
            FILE,
        )
        .expect("stray file");

        let entries = history_with(Some(&setup.root), None, &env_for(&setup)).expect("lists");

        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn a_file_saved_with_a_byte_order_mark_publishes_and_shows_its_users() {
        let setup = setup();
        let with_mark = format!("\u{feff}{FILE}");
        fs::write(&setup.source, &with_mark).expect("source is written");
        publish(&setup, &options(&setup), STAMP).expect("publishes");
        let wanted = ShowOptions {
            root: Some(&setup.root),
            team: None,
            user: None,
            platforms: &[],
        };

        let report = show_with(&wanted, &env_for(&setup)).expect("shows");

        assert_eq!(
            fs::read_to_string(setup.root.join("redirect.yaml")).expect("live reads"),
            with_mark,
            "the bytes are published as they are"
        );
        assert_eq!(report.users.len(), 1, "{:?}", report.users);
    }
}
