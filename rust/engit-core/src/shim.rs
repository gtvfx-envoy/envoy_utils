//! Publishing the launcher.
//!
//! A release of the launcher is one archive per platform, named
//! `envoy-shim-<version>-<target>.zip` (Windows) or `.tar.gz`, with a
//! `SHA256SUMS` file beside them. Publishing checks every archive against that
//! file and checks what is inside, then places each platform's files in
//! `shim/<version>/<target>/` under the config root, where installed launchers
//! look for them. When asked to move the pointer it then copies the installers
//! up to `shim/` and writes `shim/current.json`, which names the launcher
//! version every installed launcher follows. The pointer comes last, so nobody
//! is ever pointed at a launcher that is not there yet.
//!
//! A published version is never changed, and nothing is downloaded: the folder
//! holds what the release published.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use envoy_core::config::redirect::is_safe_version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{EngitError, Result};
use crate::share::{
    confirm_written, create_folder, publish_root, rename_with_retry, replace_file, require_folder,
    unreadable_by_others, Env, PublishLock, LOCK_WAIT,
};

/// The folder under the config root that holds the launcher.
const SHIM_DIR: &str = "shim";

/// The file that names the launcher version installed launchers follow.
const POINTER_FILE: &str = "current.json";

/// The newest pointer layout this `engit` writes, and the newest it will replace.
const POINTER_SCHEMA: u32 = 1;

/// The empty file publishers take turns on, kept in the shim folder.
const LOCK_FILE: &str = ".shim.lock";

const CHECKSUMS_FILE: &str = "SHA256SUMS";

const ARCHIVE_PREFIX: &str = "envoy-shim-";

/// What `--envoy-shim-version` prints before the version.
const REPORT_PREFIX: &str = "envoy-shim ";

/// The installers that go up to the shim folder, in the order they are listed.
const INSTALLERS: [&str; 4] = [
    "install-envoy.ps1",
    "uninstall-envoy.ps1",
    "install-envoy.sh",
    "uninstall-envoy.sh",
];

/// How long a launcher gets to say which version it is.
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait, once the launcher has ended, for the last of what it
/// printed. A program it started and left running can keep its output open far
/// longer than that.
const REPORT_GRACE: Duration = Duration::from_secs(2);

/// What a launcher says about its version is one short line.
const MAX_REPORT_BYTES: usize = 64 * 1024;

/// What an archive may hold before it is not a launcher archive.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// The archive file itself. A launcher archive is a few megabytes.
    archive_bytes: u64,
    /// One file in it.
    entry_bytes: u64,
    /// All the files in it.
    total_bytes: u64,
    /// Files, folders and links.
    entries: usize,
    /// One name in it.
    name_bytes: usize,
}

const LIMITS: Limits = Limits {
    archive_bytes: 256 * 1024 * 1024,
    entry_bytes: 128 * 1024 * 1024,
    total_bytes: 512 * 1024 * 1024,
    entries: 1000,
    name_bytes: 1024,
};

impl Limits {
    /// The most a compressed tar file can be once it is decompressed: the files,
    /// and for each entry its header, its padding and the records that carry a
    /// long name or other attributes, then the end of the archive.
    fn tar_stream_bytes(self) -> u64 {
        const PER_ENTRY: u64 = 16 * 1024;
        let entries = u64::try_from(self.entries).unwrap_or(u64::MAX);
        self.total_bytes
            .saturating_add(entries.saturating_add(2).saturating_mul(PER_ENTRY))
    }
}

/// A platform the launcher is built for, named as the folders of `shim/<version>/` name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    /// `windows-x86_64`.
    WindowsX64,
    /// `linux-x86_64-musl`.
    LinuxX64Musl,
    /// `macos-aarch64`.
    MacosArm64,
    /// `macos-x86_64`.
    MacosX64,
}

impl Target {
    /// Every platform a release builds the launcher for.
    pub const ALL: [Target; 4] = [
        Target::WindowsX64,
        Target::LinuxX64Musl,
        Target::MacosArm64,
        Target::MacosX64,
    ];

    /// The name of the platform's folder, and of its part of an archive's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::WindowsX64 => "windows-x86_64",
            Self::LinuxX64Musl => "linux-x86_64-musl",
            Self::MacosArm64 => "macos-aarch64",
            Self::MacosX64 => "macos-x86_64",
        }
    }

    fn is_windows(self) -> bool {
        self == Self::WindowsX64
    }

    /// Windows archives are ZIP files; the others are compressed tar files.
    fn extension(self) -> &'static str {
        if self.is_windows() {
            "zip"
        } else {
            "tar.gz"
        }
    }

    /// The file the launcher is published as.
    fn launcher(self) -> &'static str {
        if self.is_windows() {
            "envoy.exe"
        } else {
            "envoy"
        }
    }

    /// The installers an archive for this platform carries.
    fn installers(self) -> [&'static str; 2] {
        if self.is_windows() {
            [INSTALLERS[0], INSTALLERS[1]]
        } else {
            [INSTALLERS[2], INSTALLERS[3]]
        }
    }

    /// The platform this program runs on, when a launcher is built for it.
    pub fn host() -> Option<Self> {
        if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Self::WindowsX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX64Musl)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacosArm64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacosX64)
        } else {
            None
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// What [`publish_shim`] is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct ShimOptions<'a> {
    /// The folder that holds the release's archives and its `SHA256SUMS`.
    pub directory: &'a Path,
    /// The config root, or `ENVOY_CONFIG_PUBLISH_ROOT` when `None`.
    pub output: Option<&'a Path>,
    /// The version to publish, when the folder holds more than one.
    pub version: Option<&'a str>,
    /// Copy the installers up and point installed launchers at this version.
    pub move_pointer: bool,
    /// Allow the pointer to move to a pre-release.
    pub allow_prerelease: bool,
    /// Check everything and say what would be written, without writing it.
    pub dry_run: bool,
}

/// What a publish did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimOutcome {
    /// Something was written.
    Published,
    /// A dry run: something would have been written.
    WouldPublish,
    /// The share already has all of it.
    Unchanged,
}

/// What became of one platform's launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placing {
    /// It was put on the share.
    Placed,
    /// A dry run: it would be put on the share.
    WouldPlace,
    /// It is there already, with exactly these files.
    AlreadyThere,
}

/// One platform's launcher.
#[derive(Debug, Clone)]
pub struct LauncherReport {
    pub target: Target,
    pub placing: Placing,
    pub files: usize,
    pub bytes: u64,
    /// The launcher was run and said it is the version being published.
    pub ran: bool,
}

/// What became of one installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copying {
    /// It was copied up to the shim folder.
    Copied,
    /// A dry run: it would be copied.
    WouldCopy,
    /// The shim folder has this installer already.
    Same,
}

/// What became of the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointing {
    /// It names the published version now.
    Moved,
    /// A dry run: it would name the published version.
    WouldMove,
    /// It names the published version already.
    Unchanged,
    /// It was left alone because publishing was asked not to move it.
    Left,
}

/// What a publish found and did.
#[derive(Debug, Clone)]
pub struct ShimReport {
    pub outcome: ShimOutcome,
    pub version: String,
    /// `shim/<version>` under the config root.
    pub folder: PathBuf,
    pub launchers: Vec<LauncherReport>,
    /// Platforms that have no launcher for this version on the share.
    pub missing: Vec<Target>,
    /// `None` when the pointer was not to move: the installers move with it, so
    /// they were not touched.
    pub installers: Option<Vec<(&'static str, Copying)>>,
    pub pointer: Pointing,
    /// What the pointer named before, if it named a usable version.
    pub was: Option<String>,
    pub notes: Vec<String>,
}

impl fmt::Display for ShimReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.outcome {
            ShimOutcome::Published => {
                writeln!(formatter, "Published the launcher {}.", self.version)?
            }
            ShimOutcome::WouldPublish => {
                writeln!(formatter, "Would publish the launcher {}.", self.version)?;
            }
            ShimOutcome::Unchanged => writeln!(
                formatter,
                "Nothing to publish: the launcher {} is on the share already.",
                self.version
            )?,
        }
        writeln!(formatter, "  Folder:   {}", self.folder.display())?;

        writeln!(formatter, "  Launchers:")?;
        for launcher in &self.launchers {
            let what = match launcher.placing {
                Placing::Placed => "placed",
                Placing::WouldPlace => "would be placed",
                Placing::AlreadyThere => "there already",
            };
            let ran = if launcher.ran {
                format!(", ran and reports {}", self.version)
            } else {
                String::new()
            };
            writeln!(
                formatter,
                "    {:<18}{} ({} files, {}{ran})",
                launcher.target.label(),
                what,
                launcher.files,
                size(launcher.bytes)
            )?;
        }
        if !self.missing.is_empty() {
            let names: Vec<&str> = self.missing.iter().map(|target| target.label()).collect();
            writeln!(formatter, "  None on the share for: {}", names.join(", "))?;
        }

        match &self.installers {
            None => writeln!(
                formatter,
                "  Installers: not touched (they move with the pointer)"
            )?,
            Some(installers) => {
                writeln!(formatter, "  Installers:")?;
                for (name, copying) in installers {
                    let what = match copying {
                        Copying::Copied => "copied",
                        Copying::WouldCopy => "would be copied",
                        Copying::Same => "the same already",
                    };
                    writeln!(formatter, "    {name:<22}{what}")?;
                }
            }
        }

        let was = self.was.clone().unwrap_or_else(|| "none".to_string());
        match self.pointer {
            Pointing::Moved => writeln!(
                formatter,
                "  Pointer:  {POINTER_FILE} now names {} (before: {was})",
                self.version
            )?,
            Pointing::WouldMove => writeln!(
                formatter,
                "  Pointer:  {POINTER_FILE} would name {} (now: {was})",
                self.version
            )?,
            Pointing::Unchanged => writeln!(
                formatter,
                "  Pointer:  {POINTER_FILE} names {} already",
                self.version
            )?,
            Pointing::Left => match &self.was {
                Some(was) => writeln!(
                    formatter,
                    "  Pointer:  not moved: it names {was}, which installed launchers keep following"
                )?,
                None => writeln!(
                    formatter,
                    "  Pointer:  not moved: there is no usable pointer yet"
                )?,
            },
        }
        for note in &self.notes {
            writeln!(formatter, "Note: {note}")?;
        }
        Ok(())
    }
}

fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    // A launcher archive is megabytes at most, far below where a float loses a byte.
    let bytes_f = bytes as f64;
    if bytes_f >= KB * KB {
        format!("{:.1} MB", bytes_f / (KB * KB))
    } else if bytes_f >= KB {
        format!("{:.1} KB", bytes_f / KB)
    } else {
        format!("{bytes} bytes")
    }
}

/// How a publish is carried out, so that tests can change what it relies on.
struct Hooks<'a> {
    lock_wait: Duration,
    host: Option<Target>,
    /// Runs a launcher and returns what it printed for `--envoy-shim-version`.
    run: &'a dyn Fn(&Path) -> io::Result<String>,
    limits: Limits,
}

/// Publish a release of the launcher.
///
/// Every check is made before anything is written, and the pointer is written
/// last, so a launcher that is being published never changes what an installed
/// launcher does until all of its files are on the share. A version that is on
/// the share already must have exactly these files, because a published version
/// is never changed.
pub fn publish_shim(options: &ShimOptions<'_>) -> Result<ShimReport> {
    let hooks = Hooks {
        lock_wait: LOCK_WAIT,
        host: Target::host(),
        run: &reported_version,
        limits: LIMITS,
    };
    publish_with(options, &|name| env::var(name).ok(), &hooks)
}

/// One platform's archive, as the folder holds it.
struct Found {
    target: Target,
    version: String,
    name: String,
    path: PathBuf,
}

/// One file of an unpacked archive.
struct PackedFile {
    /// Relative to the folder it is unpacked into, with `/` between parts.
    name: String,
    /// `0o755` for a program, `0o644` for anything else.
    mode: u32,
    bytes: Vec<u8>,
}

struct Unpacked {
    files: Vec<PackedFile>,
    /// Links in the archive. They are not published.
    links: Vec<String>,
}

impl Unpacked {
    fn file(&self, name: &str) -> Option<&PackedFile> {
        self.files.iter().find(|file| file.name == name)
    }

    fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes.len() as u64).sum()
    }
}

fn publish_with(options: &ShimOptions<'_>, env: &Env, hooks: &Hooks<'_>) -> Result<ShimReport> {
    let root = publish_root(options.output, env)?;
    require_folder(&root)?;
    let shim_dir = root.join(SHIM_DIR);

    let found = find_archives(options.directory)?;
    let version = choose_version(&found, options.version, options.directory)?;
    if options.move_pointer && is_prerelease(&version) && !options.allow_prerelease {
        return Err(prerelease_refused(&version));
    }

    let sums = read_checksums(options.directory)?;
    let mut packed: Vec<(&Found, Unpacked)> = Vec::new();
    for archive in found.iter().filter(|archive| archive.version == version) {
        let bytes = read_archive(archive, &sums, hooks.limits)?;
        let unpacked = unpack(archive, &bytes, hooks.limits)?;
        check_shape(archive, &unpacked)?;
        packed.push((archive, unpacked));
    }
    let installers = collect_installers(&packed)?;

    let mut notes = Vec::new();
    let mut links: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (archive, unpacked) in &packed {
        for link in &unpacked.links {
            links
                .entry(link.as_str())
                .or_default()
                .push(archive.target.label());
        }
    }
    for (link, labels) in &links {
        let why = if *link == "en" {
            "the installer makes it on each machine"
        } else {
            "links are not put on the share"
        };
        notes.push(format!(
            "The link {link} in the {} archive{} is not published: {why}.",
            labels.join(", "),
            if labels.len() == 1 { "" } else { "s" }
        ));
    }

    let mut ran = BTreeSet::new();
    match hooks
        .host
        .and_then(|host| packed.iter().find(|(archive, _)| archive.target == host))
    {
        Some((archive, unpacked)) => {
            check_by_running(archive, unpacked, &version, hooks.run)?;
            ran.insert(archive.target);
        }
        None => notes.push(
            "No launcher in the folder runs on this machine, so none was run to check its \
version. The checksums were checked."
                .to_string(),
        ),
    }

    let version_dir = shim_dir.join(&version);
    let pointer_path = shim_dir.join(POINTER_FILE);

    // Publishers take turns, and look again once it is theirs.
    let _turn = if options.dry_run {
        None
    } else {
        create_folder(&shim_dir)?;
        let turn = PublishLock::acquire(&shim_dir.join(LOCK_FILE), hooks.lock_wait)?;
        notes.extend(turn.note.clone());
        Some(turn)
    };

    let mut to_place = Vec::new();
    for (archive, unpacked) in &packed {
        let destination = version_dir.join(archive.target.label());
        match compare_existing(&destination, &unpacked.files)? {
            Existing::Absent => to_place.push(archive.target),
            Existing::Same => {}
            Existing::Differs(why) => {
                return Err(EngitError::Shim(format!(
                    "{} is on the share already and is not the {} in {}: {why}. A published \
version is never changed: publish a new version instead.",
                    destination.display(),
                    archive.target,
                    archive.name
                )));
            }
        }
    }

    let before = read_pointer(&pointer_path)?;
    let was = match &before {
        PointerState::Names(version) => Some(version.clone()),
        _ => None,
    };
    let pointer_names_it = was.as_deref() == Some(version.as_str());

    let mut copying = Vec::new();
    if options.move_pointer {
        for (name, bytes) in &installers {
            let same = fs::read(shim_dir.join(name)).is_ok_and(|current| &current == bytes);
            copying.push((
                *name,
                if same {
                    Copying::Same
                } else if options.dry_run {
                    Copying::WouldCopy
                } else {
                    Copying::Copied
                },
            ));
        }
    }

    if options.move_pointer {
        if let PointerState::Unusable(why) = &before {
            notes.push(format!(
                "The pointer {} was not usable ({why}). It is replaced.",
                pointer_path.display()
            ));
        }
    }

    // Everything is checked, so nothing below fails for a reason that could have
    // been found before the first write.
    if !options.dry_run {
        if !to_place.is_empty() {
            create_folder(&version_dir)?;
        }
        for (archive, unpacked) in &packed {
            if to_place.contains(&archive.target) {
                place(&version_dir, archive.target, &unpacked.files)?;
            }
        }
        if options.move_pointer {
            for ((name, bytes), (_, how)) in installers.iter().zip(&copying) {
                if *how == Copying::Copied {
                    let path = shim_dir.join(name);
                    replace_file(&shim_dir, &path, bytes)?;
                    #[cfg(test)]
                    tests::after_replace(&path);
                    confirm_written(&path, bytes)?;
                }
            }
            if !pointer_names_it {
                let bytes = pointer_bytes(&version);
                replace_file(&shim_dir, &pointer_path, bytes.as_bytes())?;
                #[cfg(test)]
                tests::after_replace(&pointer_path);
                confirm_written(&pointer_path, bytes.as_bytes())?;
                notes.extend(unreadable_by_others(&pointer_path));
            }
        }
    }

    let mut on_share = targets_on(&version_dir);
    if options.dry_run {
        on_share.extend(to_place.iter().copied());
    }
    let missing: Vec<Target> = Target::ALL
        .into_iter()
        .filter(|target| !on_share.contains(target))
        .collect();

    if options.move_pointer {
        if let Some(earlier) = was.as_deref().filter(|earlier| *earlier != version) {
            let lost: Vec<&str> = targets_on(&shim_dir.join(earlier))
                .difference(&on_share)
                .map(|target| target.label())
                .collect();
            if !lost.is_empty() {
                notes.push(format!(
                    "{} had a launcher in {earlier}, which the pointer named until now, and have \
none in {version}. Installed launchers there will stop updating, and new installs there will fail.",
                    lost.join(", ")
                ));
            }
        }
    }

    let launchers: Vec<LauncherReport> = packed
        .iter()
        .map(|(archive, unpacked)| LauncherReport {
            target: archive.target,
            placing: match (to_place.contains(&archive.target), options.dry_run) {
                (false, _) => Placing::AlreadyThere,
                (true, true) => Placing::WouldPlace,
                (true, false) => Placing::Placed,
            },
            files: unpacked.files.len(),
            bytes: unpacked.bytes(),
            ran: ran.contains(&archive.target),
        })
        .collect();
    let pointer = match (options.move_pointer, pointer_names_it, options.dry_run) {
        (false, _, _) => Pointing::Left,
        (true, true, _) => Pointing::Unchanged,
        (true, false, true) => Pointing::WouldMove,
        (true, false, false) => Pointing::Moved,
    };
    let changed = !to_place.is_empty()
        || copying.iter().any(|(_, how)| *how != Copying::Same)
        || matches!(pointer, Pointing::Moved | Pointing::WouldMove);
    let outcome = match (changed, options.dry_run) {
        (false, _) => ShimOutcome::Unchanged,
        (true, true) => ShimOutcome::WouldPublish,
        (true, false) => ShimOutcome::Published,
    };

    Ok(ShimReport {
        outcome,
        version,
        folder: version_dir,
        launchers,
        missing,
        installers: options.move_pointer.then_some(copying),
        pointer,
        was,
        notes,
    })
}

fn prerelease_refused(version: &str) -> EngitError {
    EngitError::Shim(format!(
        "{version} is a pre-release, and publishing moves the pointer that every installed \
launcher follows within a day. Add --allow-prerelease to move it anyway, or --no-pointer to put \
the launcher on the share without moving the pointer."
    ))
}

/// A version name without a leading `v` and without build metadata, as a
/// launcher compares versions.
fn release_of(version: &str) -> &str {
    let version = version.trim().trim_start_matches(['v', 'V']);
    version
        .split_once('+')
        .map_or(version, |(release, _metadata)| release)
}

fn same_version(first: &str, second: &str) -> bool {
    release_of(first) == release_of(second)
}

fn is_prerelease(version: &str) -> bool {
    release_of(version).contains('-')
}

/// `X.Y.Z`, with an optional `-` and a pre-release name, and a `v` before it if you like.
fn is_release_version(version: &str) -> bool {
    let bare = version.strip_prefix(['v', 'V']).unwrap_or(version);
    let (core, pre) = match bare.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (bare, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        && pre.is_none_or(|pre| {
            !pre.is_empty() && pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        })
        && is_safe_version(version)
}

/// The launcher archives in a folder, ordered by version and platform.
fn find_archives(directory: &Path) -> Result<Vec<Found>> {
    if !directory.is_dir() {
        return Err(EngitError::Shim(format!(
            "{} is not a folder. Name the folder that holds the release's launcher archives.",
            directory.display()
        )));
    }
    let mut found = Vec::new();
    let listing = fs::read_dir(directory).map_err(|source| EngitError::io(directory, source))?;
    for item in listing {
        let item = item.map_err(|source| EngitError::io(directory, source))?;
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !item.path().is_file() {
            continue;
        }
        if let Some((target, version)) = parse_archive_name(&name) {
            found.push(Found {
                target,
                version,
                name,
                path: item.path(),
            });
        }
    }
    found.sort_by(|first, second| {
        first
            .version
            .cmp(&second.version)
            .then(first.target.cmp(&second.target))
    });
    Ok(found)
}

/// The platform and version an archive's name gives, or `None` for a file that
/// is not a launcher archive of a release.
fn parse_archive_name(name: &str) -> Option<(Target, String)> {
    let rest = name.strip_prefix(ARCHIVE_PREFIX)?;
    Target::ALL.into_iter().find_map(|target| {
        let suffix = format!("-{}.{}", target.label(), target.extension());
        let version = rest.strip_suffix(&suffix)?;
        is_release_version(version).then(|| (target, version.to_string()))
    })
}

fn choose_version(found: &[Found], wanted: Option<&str>, directory: &Path) -> Result<String> {
    let versions: BTreeSet<&str> = found
        .iter()
        .map(|archive| archive.version.as_str())
        .collect();
    if versions.is_empty() {
        return Err(EngitError::Shim(format!(
            "{} has no launcher archives. They are named like \
envoy-shim-v0.8.0-windows-x86_64.zip or envoy-shim-v0.8.0-linux-x86_64-musl.tar.gz, and come from \
the release's assets.",
            directory.display()
        )));
    }
    let listed = versions.iter().copied().collect::<Vec<_>>().join(", ");
    match wanted {
        Some(wanted) => versions
            .iter()
            .find(|version| same_version(version, wanted))
            .map(|version| (*version).to_string())
            .ok_or_else(|| {
                EngitError::Shim(format!(
                    "{} has no launcher archives for {}. It has {listed}.",
                    directory.display(),
                    wanted.trim()
                ))
            }),
        None if versions.len() == 1 => Ok(listed),
        None => Err(EngitError::Shim(format!(
            "{} has launcher archives for several versions ({listed}). Name the one to publish \
with --version.",
            directory.display()
        ))),
    }
}

fn read_checksums(directory: &Path) -> Result<BTreeMap<String, String>> {
    let path = directory.join(CHECKSUMS_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(EngitError::Shim(format!(
                "{} has no {CHECKSUMS_FILE}. Keep it with the archives, from the same release: it \
is how a publish knows they are the files the release built.",
                directory.display()
            )));
        }
        Err(source) => return Err(EngitError::io(&path, source)),
    };
    let text = decode_text(&bytes);

    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let not_a_sum = || {
            EngitError::Shim(format!(
                "{} has a line that is not a SHA-256 sum: {}. It should be a 64 digit hash, a \
space and the name of a file, as sha256sum writes it, in a UTF-8 or UTF-16 text file.",
                path.display(),
                shown(line)
            ))
        };
        let (hash, name) = line.split_once(char::is_whitespace).ok_or_else(not_a_sum)?;
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(not_a_sum());
        }
        // `sha256sum` marks a file it read as binary with an asterisk.
        let name = name.trim_start().trim_start_matches('*');
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let hash = hash.to_ascii_lowercase();
        if sums
            .insert(name.to_string(), hash.clone())
            .is_some_and(|earlier| earlier != hash)
        {
            return Err(EngitError::Shim(format!(
                "{} lists {name} twice, with different sums.",
                path.display()
            )));
        }
    }
    Ok(sums)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A text file as the tools of a person write it: UTF-8 with or without a byte
/// order mark, or UTF-16 with one (Windows PowerShell 5.1 writes UTF-16 from
/// `Out-File` and `>`).
fn decode_text(bytes: &[u8]) -> String {
    fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
        // A last byte on its own is not a character.
        let (pairs, _) = bytes.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|pair| unit(*pair)).collect();
        String::from_utf16_lossy(&units)
    }

    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// A name or a line from a file, cut short enough to put in a message.
fn shown(text: &str) -> String {
    const MOST: usize = 80;
    match text.char_indices().nth(MOST) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

/// Read an archive and check it against the release's sums.
fn read_archive(
    archive: &Found,
    sums: &BTreeMap<String, String>,
    limits: Limits,
) -> Result<Vec<u8>> {
    let metadata =
        fs::metadata(&archive.path).map_err(|source| EngitError::io(&archive.path, source))?;
    if metadata.len() > limits.archive_bytes {
        return Err(EngitError::Shim(format!(
            "{} is {} bytes, far more than a launcher archive. Is it the right file?",
            archive.name,
            metadata.len()
        )));
    }
    let bytes = fs::read(&archive.path).map_err(|source| EngitError::io(&archive.path, source))?;

    let expected = sums.get(&archive.name).ok_or_else(|| {
        EngitError::Shim(format!(
            "{CHECKSUMS_FILE} does not list {}. Is it from the same release?",
            archive.name
        ))
    })?;
    let actual = sha256_hex(&bytes);
    if &actual != expected {
        return Err(EngitError::Shim(format!(
            "{} does not match {CHECKSUMS_FILE}: the release says {expected} and the file is \
{actual}. Download it again.",
            archive.name
        )));
    }
    Ok(bytes)
}

/// The refusal for an archive that is not the size a launcher archive is.
fn too_big(archive: &str, what: &str) -> EngitError {
    EngitError::Shim(format!(
        "{archive} is not a launcher archive: {what} makes it far bigger than one is."
    ))
}

/// What an archive has used up of its [`Limits`].
struct Budget<'a> {
    archive: &'a str,
    limits: Limits,
    entries: usize,
    used: u64,
}

impl<'a> Budget<'a> {
    fn new(archive: &'a str, limits: Limits) -> Self {
        Self {
            archive,
            limits,
            entries: 0,
            used: 0,
        }
    }

    /// Count an entry of any kind, which is memory too: a folder or a link has a
    /// name that is kept.
    fn entry(&mut self, name: &str) -> Result<()> {
        self.entries += 1;
        if self.entries > self.limits.entries || name.len() > self.limits.name_bytes {
            return Err(too_big(self.archive, &shown(name)));
        }
        Ok(())
    }

    /// Read a file, which may not be bigger than what is left of the archive's
    /// allowance. What counts is what is read, not what the archive says it holds.
    fn read(&mut self, name: &str, reader: &mut impl Read) -> Result<Vec<u8>> {
        let allowance = self
            .limits
            .entry_bytes
            .min(self.limits.total_bytes.saturating_sub(self.used));

        let mut bytes = Vec::new();
        reader
            .take(allowance.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| {
                EngitError::Shim(format!(
                    "{}: cannot read {}: {error}",
                    self.archive,
                    shown(name)
                ))
            })?;
        if bytes.len() as u64 > allowance {
            return Err(too_big(self.archive, &shown(name)));
        }
        self.used += bytes.len() as u64;
        Ok(bytes)
    }
}

/// A reader that fails, rather than ends, when more than its allowance is asked
/// of it, so that a stream that is too big can be told from one that was cut
/// short.
struct Bounded<R> {
    inner: R,
    left: u64,
    exceeded: Rc<Cell<bool>>,
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // At exactly the allowance the stream may still end. More is too much.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => {
                    self.exceeded.set(true);
                    Err(io::Error::other("the stream is bigger than it may be"))
                }
            };
        }
        let room = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.inner.read(&mut buffer[..room])?;
        self.left -= read as u64;
        Ok(read)
    }
}

/// A name from an archive that can safely become a path under a folder.
fn safe_name(archive: &str, raw: &str) -> Result<String> {
    let mut parts: Vec<&str> = raw.split(['/', '\\']).collect();
    // Some archivers write a leading "./".
    if parts.first() == Some(&".") && parts.len() > 1 {
        parts.remove(0);
    }
    let safe = parts.iter().all(|part| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && !part.contains(':')
            && !part.chars().any(char::is_control)
            // Windows drops a trailing dot or space, so such a name is another file's.
            && !part.ends_with(['.', ' '])
            && !is_device_name(part)
    });
    if !safe {
        return Err(EngitError::Shim(format!(
            "{archive} holds {:?}, a name that cannot be unpacked safely: it could land \
outside the folder, or a system could read it as another file or as a device.",
            shown(raw)
        )));
    }
    Ok(parts.join("/"))
}

/// A name Windows keeps for a device, with or without an extension.
fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|number| matches!(number.as_bytes(), [b'1'..=b'9']))
    };
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

fn unpack(archive: &Found, bytes: &[u8], limits: Limits) -> Result<Unpacked> {
    let unpacked = if archive.target.is_windows() {
        unpack_zip(&archive.name, bytes, limits)?
    } else {
        unpack_tar(&archive.name, bytes, limits)?
    };
    // Windows and macOS keep one file for names that differ only in case.
    let mut seen = BTreeSet::new();
    for file in &unpacked.files {
        if !seen.insert(file.name.to_lowercase()) {
            return Err(EngitError::Shim(format!(
                "{} holds {} twice, or two names that differ only in case.",
                archive.name,
                shown(&file.name)
            )));
        }
    }
    Ok(unpacked)
}

/// The number of entries a ZIP file says it holds, from the record at its end,
/// or `None` when there is no such record. A file with more than 65534 entries
/// says 65535, which is far more than a launcher archive has.
fn declared_zip_entries(bytes: &[u8]) -> Option<usize> {
    const SIGNATURE: [u8; 4] = *b"PK\x05\x06";
    const RECORD: usize = 22;

    // The record is the last thing in the file, except for a comment of at most 65535 bytes.
    let last = bytes.len().checked_sub(RECORD)?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    (first..=last)
        .rev()
        .find(|&at| bytes[at..at + 4] == SIGNATURE)
        .map(|at| usize::from(u16::from_le_bytes([bytes[at + 10], bytes[at + 11]])))
}

fn unpack_zip(archive: &str, bytes: &[u8], limits: Limits) -> Result<Unpacked> {
    let broken = |error: &dyn fmt::Display| {
        EngitError::Shim(format!("{archive} cannot be read as a ZIP file: {error}"))
    };
    // What the end of the file says it holds, before the directory of it is read.
    let declared = declared_zip_entries(bytes);
    if declared.is_some_and(|declared| declared > limits.entries) {
        return Err(too_big(archive, "the number of entries in it"));
    }
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|error| broken(&error))?;
    // A name that is listed twice is read as one entry, the later, as many tools
    // do, so which file is meant is not clear. The count shows it.
    if declared.is_some_and(|declared| declared != zip.len()) {
        return Err(EngitError::Shim(format!(
            "{archive} lists a name more than once, so which file it holds under that name is \
not clear."
        )));
    }
    let mut budget = Budget::new(archive, limits);
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|error| broken(&error))?;
        budget.entry(entry.name())?;
        if entry.is_dir() {
            continue;
        }
        let unix_mode = entry.unix_mode();
        if unix_mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000) {
            return Err(EngitError::Shim(format!(
                "{archive} holds the link {}, and a link is not published.",
                shown(entry.name())
            )));
        }
        let name = safe_name(archive, entry.name())?;
        let mode = if unix_mode.is_some_and(|mode| mode & 0o111 != 0) {
            0o755
        } else {
            0o644
        };
        let bytes = budget.read(&name, &mut entry)?;
        files.push(PackedFile { name, mode, bytes });
    }
    Ok(Unpacked {
        files,
        links: Vec::new(),
    })
}

fn unpack_tar(archive: &str, bytes: &[u8], limits: Limits) -> Result<Unpacked> {
    // The tar crate reads some records (a long name, extended attributes) whole
    // into memory before anything here can count them, so the decompressed
    // stream itself is bounded, and a stream that is too big is told from one
    // that was cut short.
    let exceeded = Rc::new(Cell::new(false));
    let broken = |error: &dyn fmt::Display| {
        if exceeded.get() {
            too_big(archive, "what it holds")
        } else {
            EngitError::Shim(format!(
                "{archive} cannot be read as a compressed tar file: {error}"
            ))
        }
    };
    let stream = Bounded {
        inner: flate2::read::GzDecoder::new(bytes),
        left: limits.tar_stream_bytes(),
        exceeded: Rc::clone(&exceeded),
    };
    let mut tar = tar::Archive::new(stream);
    let mut budget = Budget::new(archive, limits);
    let (mut files, mut links) = (Vec::new(), Vec::new());
    for entry in tar.entries().map_err(|error| broken(&error))? {
        let mut entry = entry.map_err(|error| broken(&error))?;
        let kind = entry.header().entry_type();
        let raw = entry
            .path()
            .map_err(|error| broken(&error))?
            .to_string_lossy()
            .into_owned();
        budget.entry(&raw)?;
        match kind {
            tar::EntryType::Directory => continue,
            tar::EntryType::Symlink => {
                links.push(raw);
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            other => {
                return Err(EngitError::Shim(format!(
                    "{archive} holds {}, which is not a plain file ({other:?}). A launcher \
archive holds files only, apart from the link en in a tar.",
                    shown(&raw)
                )));
            }
        }
        let name = safe_name(archive, &raw)?;
        let mode = if entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0) {
            0o755
        } else {
            0o644
        };
        let bytes = budget.read(&name, &mut entry)?;
        files.push(PackedFile { name, mode, bytes });
    }
    Ok(Unpacked { files, links })
}

/// An archive of the launcher has the launcher and its installers. A file that
/// lacks them is not one, or is cut short.
fn check_shape(archive: &Found, unpacked: &Unpacked) -> Result<()> {
    let launcher = archive.target.launcher();
    let [install, uninstall] = archive.target.installers();
    for name in [launcher, install, uninstall] {
        if unpacked.file(name).is_none() {
            return Err(EngitError::Shim(format!(
                "{} has no {name}. A launcher archive holds the launcher and its installers: is \
this the right file?",
                archive.name
            )));
        }
    }
    if unpacked
        .file(launcher)
        .is_some_and(|file| file.bytes.is_empty())
    {
        return Err(EngitError::Shim(format!(
            "The {launcher} in {} is empty.",
            archive.name
        )));
    }
    Ok(())
}

/// The installers that go up to the shim folder: one of each name, which must
/// be the same in every archive that carries it.
fn collect_installers(packed: &[(&Found, Unpacked)]) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let mut installers = Vec::new();
    for name in INSTALLERS {
        let mut carriers = packed
            .iter()
            .filter(|(archive, _)| archive.target.installers().contains(&name))
            .filter_map(|(archive, unpacked)| {
                unpacked
                    .file(name)
                    .map(|file| (archive.name.as_str(), &file.bytes))
            });
        let Some((first_archive, first)) = carriers.next() else {
            continue;
        };
        for (archive, other) in carriers {
            if other != first {
                return Err(EngitError::Shim(format!(
                    "{name} is not the same in {first_archive} and {archive}, so there is no \
telling which to put on the share."
                )));
            }
        }
        installers.push((name, first.clone()));
    }
    Ok(installers)
}

/// Run the launcher that runs on this machine and check the version it reports.
///
/// An installed launcher does the same to a launcher it is about to install and
/// refuses one that does not run or reports another version, so publishing such
/// a launcher would publish one nobody can take.
fn check_by_running(
    archive: &Found,
    unpacked: &Unpacked,
    version: &str,
    run: &dyn Fn(&Path) -> io::Result<String>,
) -> Result<()> {
    let name = archive.target.launcher();
    let Some(launcher) = unpacked.file(name) else {
        return Ok(());
    };
    let folder = tempfile::tempdir().map_err(|source| EngitError::io(env::temp_dir(), source))?;
    let path = folder.path().join(name);
    write_new(&path, &launcher.bytes, 0o755).map_err(|source| EngitError::io(&path, source))?;

    let said = run(&path).map_err(|error| {
        let hint = if could_not_start(&error) {
            " The launcher is run from the temporary folder. If programs may not run there (a \
mount that does not allow it, a virus scanner, a policy), set TMPDIR (TEMP on Windows) to a \
folder where they may and publish again."
        } else {
            ""
        };
        EngitError::Shim(format!(
            "The launcher in {} does not run here: {error}. It was not published.{hint}",
            archive.name
        ))
    })?;
    let said = said.trim();
    let reported = said.strip_prefix(REPORT_PREFIX).unwrap_or(said);
    if !same_version(reported, version) {
        return Err(EngitError::Shim(format!(
            "The launcher in {} reports {:?}, not {version}. It was not published.",
            archive.name,
            shown(said)
        )));
    }
    Ok(())
}

/// Ask a launcher which version it is.
fn reported_version(path: &Path) -> io::Result<String> {
    report_of(version_command(path), RUN_TIMEOUT, REPORT_GRACE)
}

/// `true` when an error from running a launcher says that it could not be
/// started at all, as the system reports it (or, on Windows, a file that is no
/// longer there, as a virus scanner leaves one), and not that it ran and failed
/// or was too slow.
fn could_not_start(error: &io::Error) -> bool {
    error.raw_os_error().is_some() || error.kind() == io::ErrorKind::NotFound
}

/// The command that makes a launcher say which version it is.
fn version_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.arg("--envoy-shim-version");
    command
}

/// Run a command and return what it prints.
///
/// The text is read on another thread, and no more of it than a version needs,
/// so a launcher that floods its output cannot fill memory. Once the program
/// has ended only `grace` is spent waiting for the rest of what it printed,
/// because a program it started and left running can keep its output open.
fn report_of(mut command: Command, run: Duration, grace: Duration) -> io::Result<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");

    let text = Arc::new(Mutex::new(Vec::new()));
    let (finished, finished_rx) = mpsc::channel();
    {
        let text = Arc::clone(&text);
        thread::spawn(move || {
            read_some(stdout, MAX_REPORT_BYTES, &text);
            let _ = finished.send(());
        });
    }

    let give_up = Instant::now() + run;
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => {
                let _ = finished_rx.recv_timeout(grace);
                let text = text.lock().unwrap_or_else(PoisonError::into_inner);
                return Ok(String::from_utf8_lossy(&text).into_owned());
            }
            Some(status) => {
                return Err(io::Error::other(format!("it exited with {status}")));
            }
            None if Instant::now() >= give_up => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("it did not answer within {} seconds", run.as_secs_f32()),
                ));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Read from `source` into `into` until it ends or `most` bytes are there.
/// Reading stops there, and the source is let go, so a program that goes on
/// writing is stopped by the system.
fn read_some(mut source: impl Read, most: usize, into: &Mutex<Vec<u8>>) {
    let mut buffer = [0u8; 4096];
    while let Ok(read) = source.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let mut text = into.lock().unwrap_or_else(PoisonError::into_inner);
        let room = most.saturating_sub(text.len());
        text.extend_from_slice(&buffer[..read.min(room)]);
        if text.len() >= most {
            break;
        }
    }
}

/// What the share has at the place a platform's launcher goes.
enum Existing {
    Absent,
    /// A folder with exactly these files.
    Same,
    /// Something else, and what is different.
    Differs(String),
}

fn compare_existing(destination: &Path, files: &[PackedFile]) -> Result<Existing> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(Existing::Differs("it is not a folder".to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Existing::Absent),
        Err(source) => return Err(EngitError::io(destination, source)),
    }

    let found = read_tree(destination)?;
    let wanted: BTreeMap<&str, &[u8]> = files
        .iter()
        .map(|file| (file.name.as_str(), file.bytes.as_slice()))
        .collect();
    for (name, bytes) in &found {
        match (wanted.get(name.as_str()), bytes) {
            (None, _) => {
                return Ok(Existing::Differs(format!(
                    "it has {name}, which the archive does not"
                )));
            }
            (Some(wanted), Some(bytes)) if wanted == bytes => {}
            (Some(_), _) => {
                return Ok(Existing::Differs(format!(
                    "its {name} is not the one in the archive"
                )));
            }
        }
    }
    if let Some(name) = wanted.keys().find(|name| !found.contains_key(**name)) {
        return Ok(Existing::Differs(format!("it has no {name}")));
    }
    Ok(Existing::Same)
}

/// Every file under `root`, by name, with its content (`None` for anything that
/// is not a plain file).
fn read_tree(root: &Path) -> Result<BTreeMap<String, Option<Vec<u8>>>> {
    let mut tree = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let listing =
            fs::read_dir(&directory).map_err(|source| EngitError::io(&directory, source))?;
        for item in listing {
            let item = item.map_err(|source| EngitError::io(&directory, source))?;
            let name = format!("{prefix}{}", item.file_name().to_string_lossy());
            let kind = item
                .file_type()
                .map_err(|source| EngitError::io(item.path(), source))?;
            if kind.is_dir() {
                pending.push((item.path(), format!("{name}/")));
            } else if kind.is_file() {
                let bytes =
                    fs::read(item.path()).map_err(|source| EngitError::io(item.path(), source))?;
                tree.insert(name, Some(bytes));
            } else {
                tree.insert(name, None);
            }
        }
    }
    Ok(tree)
}

/// The platforms that have a launcher folder under `version_dir`.
fn targets_on(version_dir: &Path) -> BTreeSet<Target> {
    Target::ALL
        .into_iter()
        .filter(|target| version_dir.join(target.label()).is_dir())
        .collect()
}

/// What the pointer on the share is now.
enum PointerState {
    Missing,
    Names(String),
    /// There is a file, and a launcher could not use it.
    Unusable(String),
}

#[derive(Deserialize)]
struct Pointer {
    // Read so that a schema the launcher cannot parse (a string, say) makes the
    // pointer unusable here too, as it does for the launcher.
    #[allow(dead_code)]
    schema: Option<u32>,
    version: String,
}

/// The schema a pointer declares when it is newer than this `engit` knows,
/// whatever else is in the file: a newer layout may not have a version at all.
fn newer_schema(text: &str) -> Option<u64> {
    let pointer: serde_json::Value = serde_json::from_str(text).ok()?;
    pointer
        .get("schema")?
        .as_u64()
        .filter(|schema| *schema > u64::from(POINTER_SCHEMA))
}

fn read_pointer(path: &Path) -> Result<PointerState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(PointerState::Missing),
        Err(source) => return Err(EngitError::io(path, source)),
    };
    // The launcher reads the file as UTF-8, with a byte order mark allowed.
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    if let Some(schema) = newer_schema(text) {
        return Err(EngitError::Shim(format!(
            "{} is written for schema {schema}, but this engit understands up to schema \
{POINTER_SCHEMA}. Update engit.",
            path.display()
        )));
    }
    match serde_json::from_str::<Pointer>(text) {
        Ok(pointer) if is_safe_version(&pointer.version) => {
            Ok(PointerState::Names(pointer.version))
        }
        Ok(pointer) => Ok(PointerState::Unusable(format!(
            "it names {:?}, which is not a safe version name",
            shown(&pointer.version)
        ))),
        Err(error) => Ok(PointerState::Unusable(format!("it is not valid: {error}"))),
    }
}

/// The pointer's text. The version has been checked to be safe in a name, so
/// it needs no escaping.
fn pointer_bytes(version: &str) -> String {
    format!("{{\"schema\": {POINTER_SCHEMA}, \"version\": \"{version}\"}}\n")
}

/// Put one platform's files on the share, all at once or not at all.
///
/// They are written to a folder beside the place they go, read back, and the
/// folder is then renamed into place, so a launcher that looks never sees half
/// of a platform.
fn place(version_dir: &Path, target: Target, files: &[PackedFile]) -> Result<()> {
    let label = target.label();
    clear_staging(version_dir, label);
    let unique = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let staging = version_dir.join(format!(".{label}.{}-{unique}.staging", std::process::id()));
    let destination = version_dir.join(label);

    create_folder(&staging)?;
    let placed = copy_into(&staging, files)
        .and_then(|()| {
            #[cfg(test)]
            tests::after_copy(&staging);
            read_back(&staging, files)
        })
        .and_then(|()| {
            rename_with_retry(&staging, &destination)
                .map_err(|source| EngitError::io(&destination, source))
        });
    if placed.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    placed
}

/// What a publish that was stopped part way leaves, for this platform. The turn
/// is held, so nobody else is using it.
fn clear_staging(version_dir: &Path, label: &str) {
    let prefix = format!(".{label}.");
    let Ok(listing) = fs::read_dir(version_dir) else {
        return;
    };
    for item in listing.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".staging") {
            let _ = fs::remove_dir_all(item.path());
        }
    }
}

fn copy_into(staging: &Path, files: &[PackedFile]) -> Result<()> {
    for file in files {
        let mut path = staging.to_path_buf();
        let parts: Vec<&str> = file.name.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            path.push(part);
            if index + 1 < parts.len() {
                create_folder(&path)?;
            }
        }
        write_new(&path, &file.bytes, file.mode).map_err(|source| EngitError::io(&path, source))?;
    }
    Ok(())
}

fn read_back(staging: &Path, files: &[PackedFile]) -> Result<()> {
    for file in files {
        let path = file
            .name
            .split('/')
            .fold(staging.to_path_buf(), |path, part| path.join(part));
        match fs::read(&path) {
            Ok(found) if found == file.bytes => {}
            Ok(_) => {
                return Err(EngitError::Shim(format!(
                    "{} was copied to the share and does not read back as it was written.",
                    path.display()
                )));
            }
            Err(source) => return Err(EngitError::io(&path, source)),
        }
    }
    Ok(())
}

/// Create a file that must not be there, with these bytes.
///
/// Setting its mode is best effort: a filesystem that keeps no modes refuses,
/// and the file there is as usable as the filesystem makes anything.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    let _ = set_mode(&file, mode);
    file.sync_all()
}

#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    #[cfg(test)]
    if crate::share::faults::modes_are_refused() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use tempfile::{tempdir, TempDir};

    use super::*;

    type Act = Box<dyn Fn(&Path)>;

    thread_local! {
        static AFTER_COPY: RefCell<Option<Act>> = const { RefCell::new(None) };
    }

    /// Takes the fault away when it is dropped, even if the test panics.
    struct CopyFault;

    impl Drop for CopyFault {
        fn drop(&mut self) {
            AFTER_COPY.with(|act| *act.borrow_mut() = None);
        }
    }

    /// Done to a platform's staged files just after they were copied, as damage could.
    fn after_copy_do(act: impl Fn(&Path) + 'static) -> CopyFault {
        AFTER_COPY.with(|slot| *slot.borrow_mut() = Some(Box::new(act)));
        CopyFault
    }

    pub(super) fn after_copy(staging: &Path) {
        AFTER_COPY.with(|act| {
            if let Some(act) = &*act.borrow() {
                act(staging);
            }
        });
    }

    thread_local! {
        static AFTER_REPLACE: RefCell<Option<Act>> = const { RefCell::new(None) };
    }

    /// Takes the fault away when it is dropped, even if the test panics.
    struct ReplaceFault;

    impl Drop for ReplaceFault {
        fn drop(&mut self) {
            AFTER_REPLACE.with(|act| *act.borrow_mut() = None);
        }
    }

    /// Done to the file `name` of the shim folder just after it was replaced, as another
    /// publisher could.
    fn after_replacing_do(name: &'static str, act: impl Fn(&Path) + 'static) -> ReplaceFault {
        AFTER_REPLACE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path: &Path| {
                if path.file_name().is_some_and(|found| found == name) {
                    act(path);
                }
            }));
        });
        ReplaceFault
    }

    pub(super) fn after_replace(path: &Path) {
        AFTER_REPLACE.with(|act| {
            if let Some(act) = &*act.borrow() {
                act(path);
            }
        });
    }

    const VERSION: &str = "v0.8.0";

    struct Setup {
        _temp: TempDir,
        /// Where a release's archives are.
        release: PathBuf,
        /// The config root.
        root: PathBuf,
    }

    fn setup() -> Setup {
        let temp = tempdir().expect("tempdir is created");
        let release = temp.path().join("release");
        let root = temp.path().join("config");
        fs::create_dir_all(&release).expect("release is created");
        fs::create_dir_all(&root).expect("root is created");
        Setup {
            _temp: temp,
            release,
            root,
        }
    }

    fn zip_with(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, bytes, mode) in entries {
            let options = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            writer.start_file(*name, options).expect("entry starts");
            writer.write_all(bytes).expect("entry is written");
        }
        writer.finish().expect("zip finishes").into_inner()
    }

    /// A ZIP archive with a comment, and folders as well as files.
    fn zip_with_folders(folders: usize, comment: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("envoy.exe", options)
            .expect("entry starts");
        writer.write_all(b"launcher").expect("entry is written");
        for number in 0..folders {
            writer
                .add_directory(format!("folder{number}/"), options)
                .expect("folder is added");
        }
        writer.set_raw_comment(comment.to_vec().into_boxed_slice());
        writer.finish().expect("zip finishes").into_inner()
    }

    /// `bytes` with every `from` in it written as `to`, which is as long.
    fn rewritten(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        assert_eq!(from.len(), to.len());
        let mut out = bytes.to_vec();
        let mut at = 0;
        while at + from.len() <= out.len() {
            if out[at..at + from.len()] == *from {
                out[at..at + to.len()].copy_from_slice(to);
                at += from.len();
            } else {
                at += 1;
            }
        }
        out
    }

    /// A command that runs one line in the shell of the platform.
    fn in_shell(unix: &str, windows: &str) -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/D", "/C", windows]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", unix]);
            command
        }
    }

    enum Entry<'a> {
        File(&'a str, &'a [u8], u32),
        Link(&'a str, &'a str),
        Folder(&'a str),
        /// A file whose name is written as it is, however unsafe.
        Raw(&'a str, &'a [u8]),
        /// What a launcher archive never holds: a hard link, a device, a fifo.
        Special(&'a str, tar::EntryType),
        /// A PAX record of attributes, which describes the entry after it.
        Pax(&'a str, &'a [u8]),
    }

    /// A PAX record: its own length, then `key=value` and a newline.
    fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
        let tail: Vec<u8> = [&b" "[..], key.as_bytes(), &b"="[..], value, &b"\n"[..]].concat();
        let mut digits = 1;
        let length = loop {
            let total = tail.len() + digits;
            if total.to_string().len() == digits {
                break total;
            }
            digits += 1;
        };
        [length.to_string().as_bytes(), tail.as_slice()].concat()
    }

    fn tar_with(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Entry::Special(name, kind) => {
                    header.set_entry_type(*kind);
                    header.set_size(0);
                    header.set_mode(0o644);
                    if *kind == tar::EntryType::Link {
                        builder
                            .append_link(&mut header, name, "envoy")
                            .expect("entry is added");
                    } else {
                        header.set_cksum();
                        builder
                            .append_data(&mut header, name, io::empty())
                            .expect("entry is added");
                    }
                }
                Entry::Pax(key, value) => {
                    let record = pax_record(key, value);
                    let mut pax = tar::Header::new_ustar();
                    pax.set_entry_type(tar::EntryType::XHeader);
                    pax.set_path("PaxHeader/record").expect("path is set");
                    pax.set_size(record.len() as u64);
                    pax.set_mode(0o644);
                    pax.set_cksum();
                    builder
                        .append(&pax, record.as_slice())
                        .expect("record is added");
                }
                Entry::File(name, bytes, mode) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(bytes.len() as u64);
                    header.set_mode(*mode);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, name, *bytes)
                        .expect("file is added");
                }
                Entry::Link(name, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_mode(0o755);
                    builder
                        .append_link(&mut header, name, target)
                        .expect("link is added");
                }
                Entry::Folder(name) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, name, io::empty())
                        .expect("folder is added");
                }
                Entry::Raw(name, bytes) => {
                    // The crate refuses to write a name with `..` in it, so the name is
                    // put in the header by hand: this is how a hostile archive is built.
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o644);
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, *bytes).expect("file is added");
                }
            }
        }
        builder
            .into_inner()
            .expect("tar finishes")
            .finish()
            .expect("gzip finishes")
    }

    /// What `marker` puts in a platform's launcher, so that two releases differ.
    fn launcher_text(target: Target, marker: &str) -> Vec<u8> {
        format!("launcher {marker} for {target}").into_bytes()
    }

    /// An archive as a release builds it for `target`.
    fn archive_bytes(target: Target, marker: &str) -> Vec<u8> {
        let launcher = launcher_text(target, marker);
        if target.is_windows() {
            zip_with(&[
                ("envoy.exe", &launcher, 0o755),
                ("en.exe", &launcher, 0o755),
                ("install-envoy.ps1", b"windows installer", 0o755),
                ("uninstall-envoy.ps1", b"windows uninstaller", 0o755),
                ("verify-launcher.ps1", b"windows check", 0o755),
                ("LICENSE", b"license", 0o644),
            ])
        } else {
            tar_with(&[
                Entry::File("envoy", &launcher, 0o755),
                Entry::Link("en", "envoy"),
                Entry::File("install-envoy.sh", b"unix installer", 0o755),
                Entry::File("uninstall-envoy.sh", b"unix uninstaller", 0o755),
                Entry::File("LICENSE", b"license", 0o644),
            ])
        }
    }

    fn archive_name(target: Target, version: &str) -> String {
        format!(
            "{ARCHIVE_PREFIX}{version}-{}.{}",
            target.label(),
            target.extension()
        )
    }

    /// Write `SHA256SUMS` for every file in the release folder, as the release does.
    fn write_sums(release: &Path) {
        let mut names: Vec<String> = fs::read_dir(release)
            .expect("release lists")
            .filter_map(|item| item.ok()?.file_name().into_string().ok())
            .filter(|name| name != CHECKSUMS_FILE)
            .collect();
        names.sort();
        let lines: String = names
            .iter()
            .map(|name| {
                let bytes = fs::read(release.join(name)).expect("file reads");
                format!("{}  {name}\n", sha256_hex(&bytes))
            })
            .collect();
        fs::write(release.join(CHECKSUMS_FILE), lines).expect("sums are written");
    }

    fn add_release(setup: &Setup, version: &str, targets: &[Target], marker: &str) {
        for target in targets {
            fs::write(
                setup.release.join(archive_name(*target, version)),
                archive_bytes(*target, marker),
            )
            .expect("archive is written");
        }
        write_sums(&setup.release);
    }

    fn release_of_all(setup: &Setup) {
        add_release(setup, VERSION, &Target::ALL, "build 1");
    }

    fn options<'a>(setup: &'a Setup) -> ShimOptions<'a> {
        ShimOptions {
            directory: &setup.release,
            output: Some(&setup.root),
            version: None,
            move_pointer: true,
            allow_prerelease: false,
            dry_run: false,
        }
    }

    fn no_run(_: &Path) -> io::Result<String> {
        panic!("no launcher should be run");
    }

    fn hooks<'a>(host: Option<Target>, run: &'a dyn Fn(&Path) -> io::Result<String>) -> Hooks<'a> {
        Hooks {
            lock_wait: Duration::from_millis(250),
            host,
            run,
            limits: LIMITS,
        }
    }

    fn publish(_setup: &Setup, options: &ShimOptions<'_>) -> Result<ShimReport> {
        publish_with(options, &|_| None, &hooks(None, &no_run))
    }

    fn message(result: Result<ShimReport>) -> String {
        result.expect_err("the publish must fail").to_string()
    }

    fn shim(setup: &Setup) -> PathBuf {
        setup.root.join(SHIM_DIR)
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

    fn pointer_text(setup: &Setup) -> String {
        fs::read_to_string(shim(setup).join(POINTER_FILE)).expect("pointer reads")
    }

    #[test]
    fn publishing_places_every_launcher_copies_the_installers_and_moves_the_pointer() {
        let setup = setup();
        release_of_all(&setup);

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.outcome, ShimOutcome::Published);
        assert_eq!(report.version, VERSION);
        assert_eq!(report.pointer, Pointing::Moved);
        assert!(report.missing.is_empty());
        let shim = shim(&setup);
        for target in Target::ALL {
            let folder = shim.join(VERSION).join(target.label());
            assert_eq!(
                fs::read(folder.join(target.launcher())).expect("launcher reads"),
                launcher_text(target, "build 1"),
                "{target}"
            );
            assert_eq!(
                fs::read(folder.join("LICENSE")).expect("license"),
                b"license"
            );
        }
        assert_eq!(
            fs::read(shim.join("install-envoy.ps1")).expect("installer"),
            b"windows installer"
        );
        assert_eq!(
            fs::read(shim.join("uninstall-envoy.sh")).expect("installer"),
            b"unix uninstaller"
        );
        assert_eq!(
            pointer_text(&setup),
            "{\"schema\": 1, \"version\": \"v0.8.0\"}\n"
        );
        // The same link in three archives is one note, not three.
        let links: Vec<&String> = report
            .notes
            .iter()
            .filter(|note| note.starts_with("The link en"))
            .collect();
        assert_eq!(links.len(), 1, "{:?}", report.notes);
        assert!(
            links[0].contains("linux-x86_64-musl, macos-aarch64, macos-x86_64 archives"),
            "{}",
            links[0]
        );
    }

    #[test]
    fn the_pointer_is_what_the_launcher_reads() {
        // The launcher parses `{ schema, version }` and looks for
        // `shim/<version>/<platform>/envoy` where the version is the pointer's.
        let text = pointer_bytes("v0.8.0-alpha.2");

        let parsed: serde_json::Value = serde_json::from_str(&text).expect("it is JSON");

        assert_eq!(parsed["schema"], 1);
        assert_eq!(parsed["version"], "v0.8.0-alpha.2");
        assert!(is_safe_version("v0.8.0-alpha.2"));
    }

    #[test]
    fn what_the_installers_and_the_launcher_look_for_is_what_is_published() {
        // These names are the contract with the launcher and its installers.
        let labels: Vec<&str> = Target::ALL.iter().map(|target| target.label()).collect();
        assert_eq!(
            labels,
            [
                "windows-x86_64",
                "linux-x86_64-musl",
                "macos-aarch64",
                "macos-x86_64"
            ]
        );
        assert_eq!(Target::WindowsX64.launcher(), "envoy.exe");
        assert_eq!(Target::MacosArm64.launcher(), "envoy");
        assert_eq!(SHIM_DIR, "shim");
        assert_eq!(POINTER_FILE, "current.json");
        assert_eq!(INSTALLERS[..2], Target::WindowsX64.installers());
        assert_eq!(INSTALLERS[2..], Target::LinuxX64Musl.installers());
    }

    #[test]
    fn publishing_the_same_release_again_changes_nothing() {
        let setup = setup();
        release_of_all(&setup);
        publish(&setup, &options(&setup)).expect("publishes");
        let before = files_under(&setup.root);

        let report = publish(&setup, &options(&setup)).expect("publishes again");

        assert_eq!(report.outcome, ShimOutcome::Unchanged);
        assert!(report
            .launchers
            .iter()
            .all(|launcher| launcher.placing == Placing::AlreadyThere));
        assert_eq!(report.pointer, Pointing::Unchanged);
        assert_eq!(files_under(&setup.root), before);
        assert!(
            report.to_string().starts_with("Nothing to publish"),
            "{report}"
        );
    }

    #[test]
    fn staging_a_release_leaves_the_installers_and_the_pointer_alone() {
        let setup = setup();
        release_of_all(&setup);
        let mut stage = options(&setup);
        stage.move_pointer = false;

        let report = publish(&setup, &stage).expect("stages");

        assert_eq!(report.outcome, ShimOutcome::Published);
        assert_eq!(report.pointer, Pointing::Left);
        assert!(report.installers.is_none());
        let files = files_under(&shim(&setup));
        assert!(
            files
                .iter()
                .all(|file| file.starts_with("v0.8.0/") || file == ".shim.lock"),
            "{files:?}"
        );
        assert!(!shim(&setup).join(POINTER_FILE).exists());
        assert!(
            report
                .to_string()
                .contains("not moved: there is no usable pointer yet"),
            "{report}"
        );
    }

    #[test]
    fn staging_over_an_existing_pointer_leaves_it_and_says_what_it_names() {
        let setup = setup();
        add_release(&setup, "v0.7.0", &[Target::WindowsX64], "old");
        publish(&setup, &options(&setup)).expect("publishes the old one");
        let before = files_under(&shim(&setup));
        add_release(&setup, VERSION, &[Target::WindowsX64], "new");
        let mut stage = options(&setup);
        stage.version = Some(VERSION);
        stage.move_pointer = false;

        let report = publish(&setup, &stage).expect("stages");

        assert_eq!(pointer_text(&setup), pointer_bytes("v0.7.0"));
        assert!(
            report
                .to_string()
                .contains("not moved: it names v0.7.0, which installed launchers keep following"),
            "{report}"
        );
        let added: Vec<String> = files_under(&shim(&setup))
            .into_iter()
            .filter(|file| !before.contains(file))
            .collect();
        assert!(
            added.iter().all(|file| file.starts_with("v0.8.0/")),
            "{added:?}"
        );
    }

    #[test]
    fn a_staged_release_is_published_for_real_by_publishing_it_again() {
        let setup = setup();
        release_of_all(&setup);
        let mut stage = options(&setup);
        stage.move_pointer = false;
        publish(&setup, &stage).expect("stages");

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.outcome, ShimOutcome::Published);
        assert!(report
            .launchers
            .iter()
            .all(|launcher| launcher.placing == Placing::AlreadyThere));
        assert_eq!(report.pointer, Pointing::Moved);
        assert!(shim(&setup).join("install-envoy.ps1").is_file());
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn the_pointer_moves_to_a_pre_release_only_when_that_is_asked_for() {
        let setup = setup();
        add_release(&setup, "v0.9.0-rc.1", &Target::ALL, "rc");

        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("pre-release"), "{error}");
        assert!(error.contains("--allow-prerelease"), "{error}");
        assert!(!shim(&setup).exists(), "nothing was written");

        let mut staging = options(&setup);
        staging.move_pointer = false;
        publish(&setup, &staging).expect("a pre-release can be staged");
        assert!(!shim(&setup).join(POINTER_FILE).exists());

        let mut allowed = options(&setup);
        allowed.allow_prerelease = true;
        let report = publish(&setup, &allowed).expect("the pointer may move to it");
        assert_eq!(report.pointer, Pointing::Moved);
        assert_eq!(pointer_text(&setup), pointer_bytes("v0.9.0-rc.1"));
    }

    #[test]
    fn what_is_a_pre_release_is_what_the_launcher_calls_one() {
        assert!(is_prerelease("v0.8.0-alpha.2"));
        assert!(is_prerelease("0.8.0-rc.1"));
        assert!(is_prerelease("v0.8.0-rc.1+g1234567"));
        assert!(!is_prerelease("v0.8.0"));
        assert!(!is_prerelease("v0.8.0+g1234567"));
    }

    #[test]
    fn a_version_that_is_on_the_share_is_never_changed() {
        let setup = setup();
        release_of_all(&setup);
        publish(&setup, &options(&setup)).expect("publishes");
        // The same version, built again with other content.
        add_release(&setup, VERSION, &Target::ALL, "build 2");
        let before = files_under(&setup.root);

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("never changed"), "{error}");
        assert!(error.contains("not the one in the archive"), "{error}");
        assert_eq!(files_under(&setup.root), before);
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn a_folder_with_a_file_the_archive_lacks_or_without_one_it_has_is_a_different_launcher() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::WindowsX64], "build 1");
        publish(&setup, &options(&setup)).expect("publishes");
        let folder = shim(&setup).join(VERSION).join("windows-x86_64");

        fs::write(folder.join("extra.txt"), "x").expect("a file is added");
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("extra.txt"), "{error}");
        fs::remove_file(folder.join("extra.txt")).expect("it is removed");

        fs::remove_file(folder.join("LICENSE")).expect("a file is removed");
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("has no LICENSE"), "{error}");
    }

    #[test]
    fn a_platform_with_no_archive_is_named_and_the_others_are_published() {
        let setup = setup();
        add_release(
            &setup,
            VERSION,
            &[Target::WindowsX64, Target::LinuxX64Musl],
            "build 1",
        );

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.outcome, ShimOutcome::Published);
        assert_eq!(report.missing, [Target::MacosArm64, Target::MacosX64]);
        assert_eq!(report.launchers.len(), 2);
        let text = report.to_string();
        assert!(
            text.contains("None on the share for: macos-aarch64, macos-x86_64"),
            "{text}"
        );
        assert!(shim(&setup).join("install-envoy.ps1").is_file());
        assert!(shim(&setup).join("install-envoy.sh").is_file());
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn a_platform_published_earlier_is_not_missing() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::WindowsX64], "build 1");
        publish(&setup, &options(&setup)).expect("publishes");
        fs::remove_file(
            setup
                .release
                .join(archive_name(Target::WindowsX64, VERSION)),
        )
        .expect("the archive is removed");
        add_release(&setup, VERSION, &[Target::MacosX64], "build 1");

        let report = publish(&setup, &options(&setup)).expect("publishes the rest");

        assert_eq!(report.missing, [Target::LinuxX64Musl, Target::MacosArm64]);
        assert_eq!(report.launchers.len(), 1);
    }

    #[test]
    fn a_pointer_that_leaves_a_platform_behind_says_so() {
        let setup = setup();
        add_release(&setup, "v0.7.0", &Target::ALL, "old");
        publish(&setup, &options(&setup)).expect("publishes the old one");
        add_release(&setup, VERSION, &[Target::WindowsX64], "new");
        let mut next = options(&setup);
        next.version = Some(VERSION);

        let report = publish(&setup, &next).expect("publishes the new one");

        let notes = report.notes.join("\n");
        assert!(notes.contains("linux-x86_64-musl"), "{notes}");
        assert!(notes.contains("macos-aarch64"), "{notes}");
        assert!(notes.contains("had a launcher in v0.7.0"), "{notes}");
        assert!(!notes.contains("windows-x86_64 had"), "{notes}");
    }

    #[test]
    fn a_checksum_that_does_not_match_stops_the_publish_and_writes_nothing() {
        let setup = setup();
        release_of_all(&setup);
        let archive = setup
            .release
            .join(archive_name(Target::LinuxX64Musl, VERSION));
        let mut bytes = fs::read(&archive).expect("archive reads");
        bytes.push(0);
        fs::write(&archive, bytes).expect("archive is changed");

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("does not match SHA256SUMS"), "{error}");
        assert!(
            error.contains("envoy-shim-v0.8.0-linux-x86_64-musl.tar.gz"),
            "{error}"
        );
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn an_archive_the_sums_do_not_list_and_a_missing_sums_file_are_refused() {
        let setup = setup();
        release_of_all(&setup);
        let sums = fs::read_to_string(setup.release.join(CHECKSUMS_FILE)).expect("sums read");
        let without: String = sums
            .lines()
            .filter(|line| !line.contains("macos-x86_64"))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(setup.release.join(CHECKSUMS_FILE), without).expect("sums are changed");

        let error = message(publish(&setup, &options(&setup)));
        assert!(
            error.contains("does not list envoy-shim-v0.8.0-macos-x86_64.tar.gz"),
            "{error}"
        );

        fs::remove_file(setup.release.join(CHECKSUMS_FILE)).expect("sums are removed");
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("has no SHA256SUMS"), "{error}");
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn sums_are_read_as_sha256sum_writes_them() {
        let setup = setup();
        let hash = "a".repeat(64);
        fs::write(
            setup.release.join(CHECKSUMS_FILE),
            format!(
                "{hash}  plain.zip\n{}  *binary.zip\n\n{hash}  ./dir/path.zip\n",
                "B".repeat(64)
            ),
        )
        .expect("sums are written");

        let sums = read_checksums(&setup.release).expect("sums read");

        assert_eq!(sums["plain.zip"], hash);
        assert_eq!(sums["binary.zip"], "b".repeat(64));
        assert_eq!(sums["path.zip"], hash);

        fs::write(setup.release.join(CHECKSUMS_FILE), "not a sum\n").expect("sums are written");
        assert!(message_of(read_checksums(&setup.release)).contains("not a SHA-256 sum"));
    }

    #[test]
    fn sums_are_read_in_utf_8_or_the_utf_16_that_windows_powershell_writes() {
        let setup = setup();
        let hash = "c".repeat(64);
        let name = "envoy-shim-v0.8.0-windows-x86_64.zip";
        let text = format!("{hash}  {name}\r\n");
        let utf16 = |mark: [u8; 2], unit: fn(u16) -> [u8; 2]| -> Vec<u8> {
            let mut bytes = mark.to_vec();
            for value in text.encode_utf16() {
                bytes.extend_from_slice(&unit(value));
            }
            bytes
        };

        for (what, bytes) in [
            ("UTF-8", text.clone().into_bytes()),
            (
                "UTF-8 with a byte order mark",
                [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
            ),
            (
                "UTF-16 little endian",
                utf16([0xFF, 0xFE], u16::to_le_bytes),
            ),
            ("UTF-16 big endian", utf16([0xFE, 0xFF], u16::to_be_bytes)),
        ] {
            fs::write(setup.release.join(CHECKSUMS_FILE), bytes).expect("sums are written");

            let sums =
                read_checksums(&setup.release).unwrap_or_else(|error| panic!("{what}: {error}"));

            assert_eq!(sums[name], hash, "{what}");
        }

        // With no mark it cannot be told from anything else. The message says what is
        // read, and shows a part of the line, not all of it.
        let unmarked: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        fs::write(setup.release.join(CHECKSUMS_FILE), unmarked).expect("sums are written");
        let error = message_of(read_checksums(&setup.release));
        assert!(error.contains("UTF-8 or UTF-16"), "{error}");
        fs::write(
            setup.release.join(CHECKSUMS_FILE),
            format!("{}\n", "z".repeat(5000)),
        )
        .expect("sums are written");
        let error = message_of(read_checksums(&setup.release));
        assert!(error.contains("..."), "{error}");
        assert!(error.chars().count() < 400, "{}", error.chars().count());
    }

    #[test]
    fn a_name_that_the_sums_list_twice_must_have_the_same_sum_both_times() {
        let setup = setup();
        let (one, two) = ("1".repeat(64), "2".repeat(64));

        fs::write(
            setup.release.join(CHECKSUMS_FILE),
            format!("{one}  a.zip\n{one}  *a.zip\n"),
        )
        .expect("sums are written");
        assert_eq!(
            read_checksums(&setup.release).expect("sums read")["a.zip"],
            one
        );

        fs::write(
            setup.release.join(CHECKSUMS_FILE),
            format!("{one}  a.zip\n{two}  a.zip\n"),
        )
        .expect("sums are written");
        let error = message_of(read_checksums(&setup.release));
        assert!(
            error.contains("lists a.zip twice, with different sums"),
            "{error}"
        );
    }

    fn message_of<T>(result: Result<T>) -> String {
        match result {
            Ok(_) => panic!("it must fail"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn archives_of_several_versions_need_a_version_and_the_version_picks_one() {
        let setup = setup();
        add_release(&setup, "v0.7.0", &[Target::WindowsX64], "old");
        add_release(&setup, VERSION, &[Target::WindowsX64], "new");

        let error = message(publish(&setup, &options(&setup)));
        assert!(
            error.contains("several versions (v0.7.0, v0.8.0)"),
            "{error}"
        );
        assert!(error.contains("--version"), "{error}");

        let mut chosen = options(&setup);
        chosen.version = Some("0.7.0");
        let report = publish(&setup, &chosen).expect("a version without its v is found");
        assert_eq!(report.version, "v0.7.0");
        assert_eq!(pointer_text(&setup), pointer_bytes("v0.7.0"));

        chosen.version = Some("v1.0.0");
        let error = message(publish(&setup, &chosen));
        assert!(error.contains("no launcher archives for v1.0.0"), "{error}");
        assert!(error.contains("It has v0.7.0, v0.8.0"), "{error}");
    }

    #[test]
    fn a_folder_with_no_launcher_archives_says_what_they_are_called() {
        let setup = setup();
        fs::write(setup.release.join("notes.txt"), "x").expect("a file is written");
        // Not releases: a test build, a link-less name, and an unknown platform.
        for name in [
            "envoy-shim-dev-windows-x86_64.zip",
            "envoy-shim-v0.8.0-windows-x86_64.tar.gz",
            "envoy-shim-v0.8.0-freebsd-x86_64.tar.gz",
            "envoy-shim-v0.8-windows-x86_64.zip",
        ] {
            fs::write(setup.release.join(name), "x").expect("a file is written");
        }

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("has no launcher archives"), "{error}");
        assert!(
            error.contains("envoy-shim-v0.8.0-windows-x86_64.zip"),
            "{error}"
        );
        let error = message(publish(
            &setup,
            &ShimOptions {
                directory: &setup.release.join("missing"),
                ..options(&setup)
            },
        ));
        assert!(error.contains("is not a folder"), "{error}");
    }

    #[test]
    fn a_name_is_parsed_for_its_platform_and_version() {
        for (name, target, version) in [
            (
                "envoy-shim-v0.8.0-windows-x86_64.zip",
                Target::WindowsX64,
                "v0.8.0",
            ),
            (
                "envoy-shim-v0.8.0-alpha.2-linux-x86_64-musl.tar.gz",
                Target::LinuxX64Musl,
                "v0.8.0-alpha.2",
            ),
            (
                "envoy-shim-0.8.0-rc.1-macos-aarch64.tar.gz",
                Target::MacosArm64,
                "0.8.0-rc.1",
            ),
            (
                "envoy-shim-v10.20.30-macos-x86_64.tar.gz",
                Target::MacosX64,
                "v10.20.30",
            ),
        ] {
            assert_eq!(
                parse_archive_name(name),
                Some((target, version.to_string())),
                "{name}"
            );
        }
        for name in [
            "envoy-shim-v0.8.0-windows-x86_64.tar.gz",
            "envoy-shim-v0.8.0-linux-x86_64-musl.zip",
            "envoy-shim-v0.8.0+g1234-windows-x86_64.zip",
            "envoy-shim-v0.8.0--windows-x86_64.zip",
            "envoy-shim-v0.8.0-windows-x86_64.zip.sig",
            "envoy-v0.8.0-windows-x86_64.zip",
        ] {
            assert_eq!(parse_archive_name(name), None, "{name}");
        }
    }

    #[test]
    fn an_archive_without_the_launcher_or_its_installers_is_refused() {
        let setup = setup();
        let windows = setup
            .release
            .join(archive_name(Target::WindowsX64, VERSION));

        fs::write(
            &windows,
            zip_with(&[
                ("envoy.exe", b"launcher", 0o755),
                ("install-envoy.ps1", b"i", 0o755),
            ]),
        )
        .expect("archive is written");
        write_sums(&setup.release);
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("has no uninstall-envoy.ps1"), "{error}");

        fs::write(
            &windows,
            zip_with(&[
                ("envoy.exe", b"", 0o755),
                ("install-envoy.ps1", b"i", 0o755),
                ("uninstall-envoy.ps1", b"u", 0o755),
            ]),
        )
        .expect("archive is written");
        write_sums(&setup.release);
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("envoy.exe in"), "{error}");
        assert!(error.contains("is empty"), "{error}");

        fs::write(&windows, b"not a zip").expect("archive is written");
        write_sums(&setup.release);
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("cannot be read as a ZIP file"), "{error}");
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn a_name_that_could_leave_the_folder_is_refused_in_either_kind_of_archive() {
        for bad in [
            "../evil",
            "..\\evil",
            "/etc/evil",
            "C:\\evil",
            "a/../../evil",
            "sub\\..\\..\\evil",
            "",
            "a//b",
        ] {
            assert!(safe_name("x", bad).is_err(), "{bad:?}");
        }
        assert_eq!(safe_name("x", "envoy.exe").expect("safe"), "envoy.exe");
        assert_eq!(safe_name("x", "./envoy").expect("safe"), "envoy");
        assert_eq!(
            safe_name("x", "licenses\\a.txt").expect("safe"),
            "licenses/a.txt"
        );

        let setup = setup();
        let zip = zip_with(&[
            ("envoy.exe", b"launcher", 0o755),
            ("install-envoy.ps1", b"i", 0o755),
            ("uninstall-envoy.ps1", b"u", 0o755),
            ("..\\..\\evil.txt", b"x", 0o644),
        ]);
        fs::write(
            setup
                .release
                .join(archive_name(Target::WindowsX64, VERSION)),
            zip,
        )
        .expect("archive is written");
        write_sums(&setup.release);
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("outside the folder"), "{error}");

        fs::remove_file(
            setup
                .release
                .join(archive_name(Target::WindowsX64, VERSION)),
        )
        .expect("removed");
        let tar = tar_with(&[
            Entry::File("envoy", b"launcher", 0o755),
            Entry::File("install-envoy.sh", b"i", 0o755),
            Entry::File("uninstall-envoy.sh", b"u", 0o755),
            Entry::Raw("../evil", b"x"),
        ]);
        fs::write(
            setup
                .release
                .join(archive_name(Target::LinuxX64Musl, VERSION)),
            tar,
        )
        .expect("archive is written");
        write_sums(&setup.release);
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("outside the folder"), "{error}");
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn a_name_windows_would_alter_or_keep_for_a_device_is_refused() {
        for bad in [
            "nul",
            "NUL.txt",
            "con",
            "Com1.dat",
            "lpt9",
            "aux.",
            "prn.exe",
            "name.",
            "name ",
            "dir/CON/file",
            "dir./file",
            "stream:evil",
        ] {
            assert!(safe_name("x", bad).is_err(), "{bad:?}");
        }
        // Names that only look like those.
        for good in [
            "console.txt",
            "com10",
            "com0",
            "nullable",
            "auxiliary",
            "lpt",
            ".hidden",
            "a.b.c",
            "licenses/NOTICE",
        ] {
            assert!(safe_name("x", good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn two_names_that_differ_only_in_case_are_refused_as_one_file_twice() {
        let setup = setup();
        let zip = zip_with(&[
            ("envoy.exe", b"launcher", 0o755),
            ("install-envoy.ps1", b"i", 0o755),
            ("uninstall-envoy.ps1", b"u", 0o755),
            ("LICENSE", b"one", 0o644),
            ("license", b"two", 0o644),
        ]);
        fs::write(
            setup
                .release
                .join(archive_name(Target::WindowsX64, VERSION)),
            zip,
        )
        .expect("archive is written");
        write_sums(&setup.release);

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("differ only in case"), "{error}");
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn a_link_in_a_compressed_tar_is_left_out_and_a_link_in_a_zip_is_refused() {
        let other = setup();
        let setup = setup();
        add_release(&setup, VERSION, &[Target::LinuxX64Musl], "build 1");

        let report = publish(&setup, &options(&setup)).expect("publishes");

        let notes = report.notes.join("\n");
        assert!(
            notes.contains("The link en in the linux-x86_64-musl archive is not published"),
            "{notes}"
        );
        let folder = shim(&setup).join(VERSION).join("linux-x86_64-musl");
        assert!(folder.join("envoy").is_file());
        assert!(
            fs::symlink_metadata(folder.join("en")).is_err(),
            "no link on the share"
        );

        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("envoy.exe", &b"launcher"[..]),
            ("install-envoy.ps1", b"i"),
            ("uninstall-envoy.ps1", b"u"),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("entry starts");
            writer.write_all(bytes).expect("entry is written");
        }
        writer
            .add_symlink(
                "en.exe",
                "envoy.exe",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("a link is added");
        let zip = writer.finish().expect("zip finishes").into_inner();
        fs::write(
            other
                .release
                .join(archive_name(Target::WindowsX64, VERSION)),
            zip,
        )
        .expect("archive is written");
        write_sums(&other.release);

        let error = message(publish(&other, &options(&other)));

        assert!(error.contains("holds the link en.exe"), "{error}");
    }

    #[test]
    fn an_archive_may_hold_folders_and_a_file_in_one_is_published_in_it() {
        let setup = setup();
        let tar = tar_with(&[
            Entry::File("envoy", b"launcher", 0o755),
            Entry::File("install-envoy.sh", b"i", 0o755),
            Entry::File("uninstall-envoy.sh", b"u", 0o755),
            Entry::Folder("licenses/"),
            Entry::File("licenses/third-party.txt", b"notices", 0o644),
        ]);
        fs::write(
            setup
                .release
                .join(archive_name(Target::LinuxX64Musl, VERSION)),
            tar,
        )
        .expect("archive is written");
        write_sums(&setup.release);

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.launchers[0].files, 4);
        let folder = shim(&setup).join(VERSION).join("linux-x86_64-musl");
        assert_eq!(
            fs::read(folder.join("licenses").join("third-party.txt")).expect("reads"),
            b"notices"
        );
        let again = publish(&setup, &options(&setup)).expect("publishes again");
        assert_eq!(again.outcome, ShimOutcome::Unchanged);
    }

    #[test]
    fn an_archive_that_is_too_big_to_be_one_is_refused() {
        let setup = setup();
        release_of_all(&setup);
        let run: &dyn Fn(&Path) -> io::Result<String> = &no_run;
        // The first Windows entry has 35 bytes, and the whole archive 126.
        for (limits, why) in [
            (
                Limits {
                    entry_bytes: 8,
                    ..LIMITS
                },
                "one entry that is too big",
            ),
            (
                Limits {
                    total_bytes: 60,
                    ..LIMITS
                },
                "entries that are too big together",
            ),
            (
                Limits {
                    entries: 2,
                    ..LIMITS
                },
                "too many entries",
            ),
            (
                Limits {
                    name_bytes: 8,
                    ..LIMITS
                },
                "a name that is too long",
            ),
        ] {
            let tight = Hooks {
                limits,
                ..hooks(None, run)
            };

            let error = publish_with(&options(&setup), &|_| None, &tight)
                .expect_err(why)
                .to_string();

            assert!(
                error.contains("is not a launcher archive"),
                "{why}: {error}"
            );
            assert!(error.contains("far bigger"), "{why}: {error}");
            assert!(!shim(&setup).exists(), "{why}");
        }

        // A file that is bigger than any launcher archive is refused before it is read,
        // and one the size of the limit is not.
        let biggest = Target::ALL
            .iter()
            .map(|target| archive_bytes(*target, "build 1").len() as u64)
            .max()
            .expect("there are platforms");
        let over = Hooks {
            limits: Limits {
                archive_bytes: biggest - 1,
                ..LIMITS
            },
            ..hooks(None, run)
        };
        let error = publish_with(&options(&setup), &|_| None, &over)
            .expect_err("a file over the limit")
            .to_string();
        assert!(
            error.contains("far more than a launcher archive"),
            "{error}"
        );
        assert!(!shim(&setup).exists());
        let at = Hooks {
            limits: Limits {
                archive_bytes: biggest,
                ..LIMITS
            },
            ..hooks(None, run)
        };
        publish_with(&options(&setup), &|_| None, &at).expect("a file at the limit is fine");
    }

    #[test]
    fn the_number_of_entries_allowed_is_the_most_and_not_one_less() {
        // The Windows archive has 6 entries, and the Unix one 5, a link among them.
        let windows = archive_bytes(Target::WindowsX64, "x");
        let unix = archive_bytes(Target::LinuxX64Musl, "x");
        let allowing = |entries| Limits { entries, ..LIMITS };

        assert!(unpack_zip("w.zip", &windows, allowing(6)).is_ok());
        assert!(message_of(unpack_zip("w.zip", &windows, allowing(5))).contains("far bigger"));
        assert!(unpack_tar("u.tar.gz", &unix, allowing(5)).is_ok());
        assert!(message_of(unpack_tar("u.tar.gz", &unix, allowing(4))).contains("far bigger"));
    }

    #[test]
    fn links_and_folders_are_entries_too_and_cost_what_a_file_does() {
        let limits = Limits {
            entries: 8,
            ..LIMITS
        };
        let names: Vec<String> = (0..20).map(|number| format!("entry{number}")).collect();

        let mut links = vec![Entry::File("envoy", b"launcher", 0o755)];
        links.extend(names.iter().map(|name| Entry::Link(name, "envoy")));
        let error = message_of(unpack_tar("l.tar.gz", &tar_with(&links), limits));
        assert!(error.contains("far bigger"), "{error}");

        let mut folders = vec![Entry::File("envoy", b"launcher", 0o755)];
        let folder_names: Vec<String> = names.iter().map(|name| format!("{name}/")).collect();
        folders.extend(folder_names.iter().map(|name| Entry::Folder(name)));
        let error = message_of(unpack_tar("f.tar.gz", &tar_with(&folders), limits));
        assert!(error.contains("far bigger"), "{error}");

        let error = message_of(unpack_zip("f.zip", &zip_with_folders(20, b""), limits));
        assert!(error.contains("far bigger"), "{error}");
        // A few are fine.
        assert!(unpack_zip("f.zip", &zip_with_folders(3, b""), limits).is_ok());
    }

    #[test]
    fn a_zip_that_says_it_holds_many_entries_is_refused_before_its_directory_is_read() {
        // Only the record at the end of the file, saying 5000 entries, and no directory
        // for it: reading the directory would fail, so the refusal is what came first.
        let mut record = b"PK\x05\x06".to_vec();
        record.extend_from_slice(&[0; 4]);
        record.extend_from_slice(&5000u16.to_le_bytes());
        record.extend_from_slice(&5000u16.to_le_bytes());
        record.extend_from_slice(&[0; 10]);

        let error = message_of(unpack_zip("big.zip", &record, LIMITS));

        assert!(error.contains("far bigger"), "{error}");
        assert!(error.contains("the number of entries"), "{error}");
    }

    #[test]
    fn the_record_at_the_end_of_a_zip_is_found_whatever_its_comment() {
        for comment in [0, 1, 1000, usize::from(u16::MAX)] {
            let zip = zip_with_folders(2, &vec![b'c'; comment]);
            assert_eq!(
                declared_zip_entries(&zip),
                Some(3),
                "a {comment} byte comment"
            );
        }
        assert_eq!(declared_zip_entries(b"not a zip"), None);
        assert_eq!(declared_zip_entries(&[]), None);
    }

    #[test]
    fn a_zip_that_lists_a_name_twice_is_refused() {
        // The archive library keeps only the later of two entries with one name, so the
        // count the file declares is the only sign. A ZIP writer refuses to write the
        // same name twice: the second name is written as another, and changed.
        let zip = zip_with(&[
            ("envoy.exe", b"launcher", 0o755),
            ("install-envoy.ps1", b"i", 0o755),
            ("uninstall-envoy.ps1", b"u", 0o755),
            ("LICENSE", b"one", 0o644),
            ("LICENSX", b"two", 0o644),
        ]);
        let twice = rewritten(&zip, b"LICENSX", b"LICENSE");

        let error = message_of(unpack_zip("twice.zip", &twice, LIMITS));

        assert!(error.contains("lists a name more than once"), "{error}");
        assert!(unpack_zip("once.zip", &zip, LIMITS).is_ok());
    }

    #[test]
    fn a_tar_with_a_record_bigger_than_the_files_could_need_is_refused_unread() {
        // The long name and the attributes of an entry are read whole, before the entry
        // is seen, so what bounds them is the size of the stream. The limits are small,
        // and the name length is not in question here.
        let limits = Limits {
            entry_bytes: 1000,
            total_bytes: 1000,
            entries: 10,
            name_bytes: usize::MAX,
            ..LIMITS
        };
        let beyond = usize::try_from(limits.tar_stream_bytes()).expect("fits") + 1000;
        let big = "a".repeat(beyond);
        let small = "a".repeat(150);

        let long_name = tar_with(&[Entry::File(&big, b"x", 0o644)]);
        let long_target = tar_with(&[Entry::Link("en", &big)]);
        let attributes = tar_with(&[Entry::Pax("comment", big.as_bytes())]);
        for (what, bytes) in [
            ("a long name", long_name),
            ("a long link target", long_target),
            ("attributes", attributes),
        ] {
            let error = message_of(unpack_tar("b.tar.gz", &bytes, limits));
            assert!(error.contains("far bigger"), "{what}: {error}");
        }

        // A name or attributes that are long, and no more, are read.
        let long_name = tar_with(&[Entry::File(&small, b"x", 0o644)]);
        let unpacked = unpack_tar("a.tar.gz", &long_name, limits).expect("a long name is fine");
        assert_eq!(unpacked.files[0].name, small);
        let attributes = tar_with(&[
            Entry::Pax("comment", small.as_bytes()),
            Entry::File("envoy", b"x", 0o755),
        ]);
        unpack_tar("a.tar.gz", &attributes, limits).expect("a few attributes are fine");
    }

    #[test]
    fn a_stream_may_end_exactly_at_its_allowance_and_not_go_past_it() {
        let read = |length: u64| {
            let exceeded = Rc::new(Cell::new(false));
            let mut stream = Bounded {
                inner: io::repeat(7).take(length),
                left: 10,
                exceeded: Rc::clone(&exceeded),
            };
            let mut bytes = Vec::new();
            let result = stream.read_to_end(&mut bytes);
            (result.is_ok(), bytes.len(), exceeded.get())
        };

        assert_eq!(read(9), (true, 9, false));
        assert_eq!(read(10), (true, 10, false));
        let (ended, _, exceeded) = read(11);
        assert!(!ended, "the 11th byte is too many");
        assert!(
            exceeded,
            "and that is said, so it is not taken for a cut stream"
        );
    }

    #[test]
    fn a_name_is_cut_to_a_line_to_show_and_never_in_the_middle_of_a_character() {
        assert_eq!(shown("short"), "short");
        let exactly = "é".repeat(80);
        assert_eq!(shown(&exactly), exactly);
        assert_eq!(shown(&"é".repeat(81)), format!("{exactly}..."));
    }

    #[test]
    fn a_name_with_a_control_character_is_refused_and_not_printed_as_it_is() {
        for bad in [
            "a\u{7}b",
            "tab\tname",
            "new\nline",
            "\u{1b}[31mred",
            "nul\0byte",
            "\u{85}next",
        ] {
            let error = message_of(safe_name("x.zip", bad));
            assert!(
                error.contains("cannot be unpacked safely"),
                "{bad:?}: {error}"
            );
            assert!(!error.chars().any(char::is_control), "{error:?}");
        }
    }

    #[test]
    fn a_tar_entry_that_is_no_file_folder_or_symbolic_link_is_refused() {
        for (kind, what) in [
            (tar::EntryType::Link, "a hard link"),
            (tar::EntryType::Char, "a character device"),
            (tar::EntryType::Block, "a block device"),
            (tar::EntryType::Fifo, "a fifo"),
        ] {
            let bytes = tar_with(&[
                Entry::File("envoy", b"launcher", 0o755),
                Entry::Special("odd", kind),
            ]);

            let error = message_of(unpack_tar("s.tar.gz", &bytes, LIMITS));

            assert!(
                error.contains("odd, which is not a plain file"),
                "{what}: {error}"
            );
        }
    }

    #[test]
    fn the_installers_must_be_the_same_in_every_archive_that_carries_them() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::LinuxX64Musl], "build 1");
        let other = tar_with(&[
            Entry::File("envoy", b"launcher", 0o755),
            Entry::File("install-envoy.sh", b"a different installer", 0o755),
            Entry::File("uninstall-envoy.sh", b"unix uninstaller", 0o755),
        ]);
        fs::write(
            setup.release.join(archive_name(Target::MacosX64, VERSION)),
            other,
        )
        .expect("archive is written");
        write_sums(&setup.release);

        let error = message(publish(&setup, &options(&setup)));

        assert!(
            error.contains("install-envoy.sh is not the same in"),
            "{error}"
        );
        assert!(!shim(&setup).exists());
    }

    #[test]
    fn the_launcher_that_runs_here_must_run_and_report_the_version() {
        let other = setup();
        let setup = setup();
        release_of_all(&setup);
        let host = Some(Target::WindowsX64);
        let say =
            |text: &'static str| move |_: &Path| -> io::Result<String> { Ok(text.to_string()) };

        let tagged = say("envoy-shim v0.8.0\n");
        let report = publish_with(&options(&setup), &|_| None, &hooks(host, &tagged))
            .expect("a launcher that reports the version is published");
        let windows = report
            .launchers
            .iter()
            .find(|launcher| launcher.target == Target::WindowsX64)
            .expect("windows is there");
        assert!(windows.ran);
        assert!(
            report
                .launchers
                .iter()
                .filter(|launcher| launcher.ran)
                .count()
                == 1
        );
        assert!(
            report.to_string().contains("ran and reports v0.8.0"),
            "{report}"
        );

        let wrong = say("envoy-shim v0.7.0\n");
        release_of_all(&other);
        let error = publish_with(&options(&other), &|_| None, &hooks(host, &wrong))
            .expect_err("a launcher that reports another version")
            .to_string();
        assert!(
            error.contains("reports \"envoy-shim v0.7.0\", not v0.8.0"),
            "{error}"
        );
        assert!(!shim(&other).exists());

        let broken = |_: &Path| -> io::Result<String> { Err(io::Error::other("it exited with 1")) };
        let error = publish_with(&options(&other), &|_| None, &hooks(host, &broken))
            .expect_err("a launcher that does not run")
            .to_string();
        assert!(
            error.contains("does not run here: it exited with 1"),
            "{error}"
        );
        assert!(!shim(&other).exists());

        // What the launcher did says nothing about the folder it was run from.
        assert!(!error.contains("TMPDIR"), "{error}");
        let slow = |_: &Path| -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "it did not answer within 15 seconds",
            ))
        };
        let error = publish_with(&options(&other), &|_| None, &hooks(host, &slow))
            .expect_err("a launcher that does not answer")
            .to_string();
        assert!(error.contains("did not answer"), "{error}");
        assert!(!error.contains("TMPDIR"), "{error}");

        // An error from the system means it could not be started, and the folder it was
        // run from may be the reason. So may a file that is gone, which is what a virus
        // scanner leaves on Windows.
        let denied = |_: &Path| -> io::Result<String> {
            Err(io::Error::from_raw_os_error(if cfg!(windows) {
                5
            } else {
                13
            }))
        };
        let gone = |_: &Path| -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::NotFound, "program not found"))
        };
        for (what, run) in [
            ("denied", &denied as &dyn Fn(&Path) -> io::Result<String>),
            ("gone", &gone),
        ] {
            let error = publish_with(&options(&other), &|_| None, &hooks(host, run))
                .expect_err(what)
                .to_string();
            assert!(error.contains("does not run here"), "{what}: {error}");
            assert!(
                error.contains("TMPDIR (TEMP on Windows)"),
                "{what}: {error}"
            );
            assert!(!shim(&other).exists(), "{what}");
        }

        // A line of any length is not printed in full.
        let long = |_: &Path| -> io::Result<String> { Ok("x".repeat(5000)) };
        let error = publish_with(&options(&other), &|_| None, &hooks(host, &long))
            .expect_err("a launcher that reports no version")
            .to_string();
        assert!(error.contains("..."), "{error}");
        assert!(error.chars().count() < 300, "{}", error.chars().count());
    }

    #[test]
    fn a_launcher_is_asked_for_its_version_the_way_the_launcher_answers_it() {
        let command = version_command(Path::new("some/envoy"));

        assert_eq!(command.get_program(), "some/envoy");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--envoy-shim-version"]
        );
    }

    #[test]
    fn what_a_program_prints_is_its_report_and_a_failure_is_not_one() {
        let said = report_of(
            in_shell("echo envoy-shim v1", "echo envoy-shim v1"),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .expect("a program that prints a line reports it");
        assert_eq!(said.trim(), "envoy-shim v1");

        let error = report_of(
            in_shell("exit 3", "exit 3"),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .expect_err("a program that fails reports nothing");
        assert!(error.to_string().contains("it exited with"), "{error}");
        assert!(
            !could_not_start(&error),
            "it ran, so it is not taken for one that could not start"
        );

        let error = report_of(
            Command::new("this-program-does-not-exist-anywhere"),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .expect_err("a program that cannot be started reports nothing");
        assert!(could_not_start(&error), "{error}");
    }

    #[test]
    fn a_program_that_does_not_answer_is_stopped() {
        let started = Instant::now();

        let error = report_of(
            in_shell("exec sleep 20", "ping -n 9 127.0.0.1 > nul"),
            Duration::from_millis(300),
            Duration::from_secs(1),
        )
        .expect_err("a program that does not answer has not reported");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        assert!(error.to_string().contains("did not answer"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it was stopped and not waited for: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_program_left_running_by_the_launcher_does_not_hold_the_publish_up() {
        // The launcher ends, and what it started goes on holding its output open, so
        // the end of the output never comes.
        let started = Instant::now();

        let said = report_of(
            in_shell(
                "echo envoy-shim v1; sleep 20 &",
                "echo envoy-shim v1 & start /b ping -n 21 127.0.0.1 > nul",
            ),
            Duration::from_secs(30),
            Duration::from_millis(300),
        )
        .expect("what it printed is its report");

        assert_eq!(said.trim(), "envoy-shim v1");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_launcher_that_never_stops_printing_is_stopped_and_not_read_whole() {
        let started = Instant::now();

        let error = report_of(
            in_shell("while :; do echo envoy-shim v1; done", ""),
            Duration::from_secs(10),
            Duration::from_secs(5),
        )
        .expect_err("a program that never stops printing is not a launcher");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{error}: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn what_a_launcher_prints_is_read_no_further_than_a_version_needs() {
        let text = Mutex::new(Vec::new());
        // Not a whole number of the reads it makes.
        read_some(io::repeat(b'v').take(1_000_000), 10_000, &text);
        assert_eq!(text.lock().expect("lock").len(), 10_000);

        let short = Mutex::new(Vec::new());
        read_some(&b"envoy-shim v1\n"[..], 10_000, &short);
        assert_eq!(short.lock().expect("lock").as_slice(), b"envoy-shim v1\n");
    }

    #[test]
    fn a_build_with_metadata_is_the_same_version_and_the_launcher_is_given_its_own_file() {
        let setup = setup();
        release_of_all(&setup);
        let seen = RefCell::new(Vec::new());
        let run = |path: &Path| -> io::Result<String> {
            seen.borrow_mut().push((
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                fs::read(path).expect("the file is there"),
            ));
            Ok("envoy-shim v0.8.0+g1234567.dirty".to_string())
        };

        publish_with(
            &options(&setup),
            &|_| None,
            &hooks(Some(Target::LinuxX64Musl), &run),
        )
        .expect("publishes");

        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "envoy");
        assert_eq!(seen[0].1, launcher_text(Target::LinuxX64Musl, "build 1"));
    }

    #[test]
    fn with_no_launcher_for_this_machine_none_is_run_and_the_report_says_so() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::MacosX64], "build 1");

        // This machine's own platform has no archive here.
        let report = publish_with(
            &options(&setup),
            &|_| None,
            &hooks(Some(Target::WindowsX64), &no_run),
        )
        .expect("publishes");

        let notes = report.notes.join("\n");
        assert!(notes.contains("none was run"), "{notes}");
        assert!(report.launchers.iter().all(|launcher| !launcher.ran));
    }

    #[test]
    fn a_dry_run_checks_everything_and_writes_nothing() {
        let setup = setup();
        release_of_all(&setup);
        let mut dry = options(&setup);
        dry.dry_run = true;

        let report = publish(&setup, &dry).expect("checks");

        assert_eq!(report.outcome, ShimOutcome::WouldPublish);
        assert_eq!(report.pointer, Pointing::WouldMove);
        assert!(report
            .launchers
            .iter()
            .all(|launcher| launcher.placing == Placing::WouldPlace));
        assert!(report.missing.is_empty());
        assert!(
            files_under(&setup.root).is_empty(),
            "{:?}",
            files_under(&setup.root)
        );
        assert!(report.to_string().starts_with("Would publish"), "{report}");

        // It refuses what a real publish refuses.
        let mut prerelease = dry;
        add_release(&setup, "v0.9.0-rc.1", &Target::ALL, "rc");
        prerelease.version = Some("v0.9.0-rc.1");
        assert!(message(publish(&setup, &prerelease)).contains("pre-release"));
        let mut tampered = options(&setup);
        tampered.version = Some(VERSION);
        publish(&setup, &tampered).expect("publishes for real");
        add_release(&setup, VERSION, &Target::ALL, "build 2");
        let mut again = dry;
        again.version = Some(VERSION);
        assert!(message(publish(&setup, &again)).contains("never changed"));
    }

    #[test]
    fn the_publish_root_is_the_publish_variable_and_never_the_runtime_one() {
        let setup = setup();
        release_of_all(&setup);
        let mut no_root = options(&setup);
        no_root.output = None;
        let variables: HashMap<&str, String> =
            HashMap::from([("ENVOY_CONFIG_PROD", setup.root.display().to_string())]);

        let error = publish_with(
            &no_root,
            &|name| variables.get(name).cloned(),
            &hooks(None, &no_run),
        )
        .expect_err("the runtime variable is not a publish root")
        .to_string();
        assert!(error.contains("ENVOY_CONFIG_PUBLISH_ROOT"), "{error}");
        assert!(files_under(&setup.root).is_empty());

        let variables: HashMap<&str, String> = HashMap::from([(
            "ENVOY_CONFIG_PUBLISH_ROOT",
            setup.root.display().to_string(),
        )]);
        publish_with(
            &no_root,
            &|name| variables.get(name).cloned(),
            &hooks(None, &no_run),
        )
        .expect("the publish variable is");
        assert!(shim(&setup).join(POINTER_FILE).is_file());
    }

    #[test]
    fn a_root_that_does_not_exist_is_refused_and_not_created() {
        let setup = setup();
        release_of_all(&setup);
        let missing = setup.root.join("nowhere");
        let mut wrong = options(&setup);
        wrong.output = Some(&missing);

        let error = message(publish(&setup, &wrong));

        assert!(error.contains("is not a folder"), "{error}");
        assert!(!missing.exists());
    }

    #[test]
    fn a_publish_waits_for_another_one_and_gives_up_when_it_does_not_finish() {
        let setup = setup();
        release_of_all(&setup);
        fs::create_dir_all(shim(&setup)).expect("shim is created");
        let held = PublishLock::acquire(&shim(&setup).join(LOCK_FILE), Duration::from_secs(1))
            .expect("the first publisher has its turn");

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("is still running"), "{error}");
        assert_eq!(
            files_under(&setup.root),
            [format!("{SHIM_DIR}/{LOCK_FILE}")]
        );
        drop(held);
        publish(&setup, &options(&setup)).expect("it goes ahead once the turn is free");
    }

    #[test]
    fn a_pointer_for_a_newer_schema_is_not_overwritten_and_a_broken_one_is() {
        let setup = setup();
        release_of_all(&setup);
        fs::create_dir_all(shim(&setup)).expect("shim is created");
        let pointer = shim(&setup).join(POINTER_FILE);

        fs::write(&pointer, "{\"schema\": 2, \"version\": \"v1.0.0\"}")
            .expect("pointer is written");
        let error = message(publish(&setup, &options(&setup)));
        assert!(error.contains("schema 2"), "{error}");
        assert!(error.contains("Update engit"), "{error}");
        assert_eq!(
            fs::read_to_string(&pointer).expect("reads"),
            "{\"schema\": 2, \"version\": \"v1.0.0\"}"
        );
        assert!(!shim(&setup).join(VERSION).exists());

        fs::write(&pointer, "{ not json").expect("pointer is written");
        let report = publish(&setup, &options(&setup)).expect("a broken pointer is replaced");
        assert!(
            report.notes.join("\n").contains("was not usable"),
            "{report}"
        );
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));

        fs::write(&pointer, "{\"version\": \"../../x\"}").expect("pointer is written");
        let mut later = options(&setup);
        later.version = Some(VERSION);
        let report = publish(&setup, &later).expect("an unsafe pointer is replaced");
        assert!(
            report.notes.join("\n").contains("not a safe version name"),
            "{report}"
        );
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn a_pointer_for_a_newer_schema_is_not_overwritten_whatever_else_it_holds() {
        // A newer layout may not have a version at all, and no launcher or engit that
        // reads schema 1 can tell what it means.
        for newer in [
            "{\"schema\": 2}",
            "{\"schema\": 2, \"current\": {\"windows\": \"v1.0.0\"}}",
            "{\"schema\": 4294967296, \"version\": \"v1.0.0\"}",
            "{\"schema\": 18446744073709551615}",
            "\u{feff}{\"schema\": 3}",
        ] {
            let setup = setup();
            release_of_all(&setup);
            fs::create_dir_all(shim(&setup)).expect("shim is created");
            let pointer = shim(&setup).join(POINTER_FILE);
            fs::write(&pointer, newer).expect("pointer is written");

            // Staging, which leaves the pointer alone, still refuses: the layout it
            // sits in may be different too.
            for move_pointer in [true, false] {
                let mut wanted = options(&setup);
                wanted.move_pointer = move_pointer;

                let error = message(publish(&setup, &wanted));

                assert!(error.contains("Update engit"), "{newer}: {error}");
                assert_eq!(
                    fs::read_to_string(&pointer).expect("reads"),
                    newer,
                    "{newer}"
                );
                assert!(!shim(&setup).join(VERSION).exists(), "{newer}");
            }
        }
    }

    #[test]
    fn a_pointer_the_launcher_cannot_read_is_replaced_even_when_its_schema_looks_newer() {
        // The launcher reads the schema as a whole number up to u32, and refuses
        // anything else as it refuses a file that is not JSON.
        for unreadable in [
            "{\"schema\": \"2\", \"version\": \"v1.0.0\"}",
            "{\"schema\": 2.5, \"version\": \"v1.0.0\"}",
            "{\"schema\": 99999999999999999999999, \"version\": \"v1.0.0\"}",
            "{\"schema\": -1, \"version\": \"v1.0.0\"}",
        ] {
            let setup = setup();
            release_of_all(&setup);
            fs::create_dir_all(shim(&setup)).expect("shim is created");
            fs::write(shim(&setup).join(POINTER_FILE), unreadable).expect("pointer is written");

            let report = publish(&setup, &options(&setup)).expect(unreadable);

            assert!(
                report.notes.join("\n").contains("was not usable"),
                "{unreadable}: {report}"
            );
            assert_eq!(pointer_text(&setup), pointer_bytes(VERSION), "{unreadable}");
        }
    }

    #[test]
    fn a_pointer_with_a_byte_order_mark_is_read_as_the_launcher_reads_it() {
        let setup = setup();
        release_of_all(&setup);
        fs::create_dir_all(shim(&setup)).expect("shim is created");
        let mut marked = vec![0xEF, 0xBB, 0xBF];
        marked.extend_from_slice(pointer_bytes(VERSION).as_bytes());
        fs::write(shim(&setup).join(POINTER_FILE), &marked).expect("pointer is written");

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.was.as_deref(), Some(VERSION));
        assert_eq!(report.pointer, Pointing::Unchanged);
        assert_eq!(
            fs::read(shim(&setup).join(POINTER_FILE)).expect("reads"),
            marked,
            "a pointer that works is left as it is"
        );
    }

    #[test]
    fn the_pointer_before_is_reported_and_a_pointer_without_a_schema_is_read() {
        let setup = setup();
        add_release(&setup, "v0.7.0", &[Target::WindowsX64], "old");
        publish(&setup, &options(&setup)).expect("publishes the old one");
        add_release(&setup, VERSION, &[Target::WindowsX64], "new");
        let mut next = options(&setup);
        next.version = Some(VERSION);

        let report = publish(&setup, &next).expect("publishes");

        assert_eq!(report.was.as_deref(), Some("v0.7.0"));
        assert!(report.to_string().contains("now names v0.8.0"), "{report}");
        assert!(report.to_string().contains("(before: v0.7.0)"), "{report}");

        fs::write(shim(&setup).join(POINTER_FILE), "{\"version\": \"v0.8.0\"}").expect("pointer");
        let report = publish(&setup, &next).expect("publishes again");
        assert_eq!(report.pointer, Pointing::Unchanged);
    }

    #[test]
    fn a_pointer_that_names_the_version_without_its_v_is_rewritten() {
        // A launcher looks in the folder named exactly as the pointer says, so
        // "0.8.0" is not "v0.8.0" even though both are the same release.
        let setup = setup();
        release_of_all(&setup);
        fs::create_dir_all(shim(&setup)).expect("shim is created");
        fs::write(
            shim(&setup).join(POINTER_FILE),
            "{\"schema\": 1, \"version\": \"0.8.0\"}",
        )
        .expect("pointer is written");

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.pointer, Pointing::Moved);
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn a_pointer_that_was_changed_underneath_the_publish_says_so() {
        let setup = setup();
        release_of_all(&setup);
        let _fault = after_replacing_do(POINTER_FILE, |pointer| {
            fs::write(pointer, "someone else\n").expect("the pointer is changed");
        });

        let error = message(publish(&setup, &options(&setup)));

        assert!(
            error.contains("does not have the content that was just published"),
            "{error}"
        );
    }

    #[test]
    fn an_installer_that_was_changed_underneath_the_publish_says_so() {
        let setup = setup();
        release_of_all(&setup);
        let _fault = after_replacing_do("install-envoy.sh", |installer| {
            fs::write(installer, "someone else\n").expect("the installer is changed");
        });

        let error = message(publish(&setup, &options(&setup)));

        assert!(
            error.contains("install-envoy.sh does not have the content that was just published"),
            "{error}"
        );
        assert!(
            !shim(&setup).join(POINTER_FILE).exists(),
            "the pointer is moved last"
        );
    }

    #[test]
    fn a_file_where_a_launcher_folder_goes_is_not_replaced_by_one() {
        let setup = setup();
        release_of_all(&setup);
        let version_dir = shim(&setup).join(VERSION);
        fs::create_dir_all(&version_dir).expect("version folder is created");
        fs::write(version_dir.join("windows-x86_64"), "in the way").expect("file is written");

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("it is not a folder"), "{error}");
        assert!(error.contains("never changed"), "{error}");
        assert_eq!(
            fs::read_to_string(version_dir.join("windows-x86_64")).expect("reads"),
            "in the way"
        );
        assert!(!shim(&setup).join(POINTER_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_where_a_launcher_folder_goes_is_not_followed() {
        let setup = setup();
        release_of_all(&setup);
        let elsewhere = setup.root.join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("elsewhere is created");
        let version_dir = shim(&setup).join(VERSION);
        fs::create_dir_all(&version_dir).expect("version folder is created");
        std::os::unix::fs::symlink(&elsewhere, version_dir.join("windows-x86_64"))
            .expect("link is made");

        let error = message(publish(&setup, &options(&setup)));

        assert!(error.contains("it is not a folder"), "{error}");
        assert!(
            files_under(&elsewhere).is_empty(),
            "nothing is written through it"
        );
    }

    #[test]
    fn a_folder_that_cannot_lock_files_is_published_to_and_says_so() {
        let setup = setup();
        release_of_all(&setup);
        let _fault = crate::share::faults::unlockable();

        let report = publish(&setup, &options(&setup)).expect("publishes");

        assert_eq!(report.outcome, ShimOutcome::Published);
        let notes = report.notes.join("\n");
        assert!(notes.contains("cannot lock files"), "{notes}");
        assert!(notes.contains("nothing stopped another publish"), "{notes}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pointer_other_users_cannot_read_is_reported() {
        use std::os::unix::fs::PermissionsExt;

        let setup = setup();
        release_of_all(&setup);
        // As if the filesystem had refused to set the mode under a strict umask.
        let _fault = after_replacing_do(POINTER_FILE, |pointer| {
            fs::set_permissions(pointer, fs::Permissions::from_mode(0o600)).expect("mode is set");
        });

        let report = publish(&setup, &options(&setup)).expect("publishes");

        let note = report.notes.join("\n");
        assert!(note.contains("has mode 600"), "{note}");
        assert!(note.contains("Check that they can"), "{note}");
    }

    #[test]
    fn what_a_publish_that_was_killed_left_behind_is_cleared() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::WindowsX64], "build 1");
        let version_dir = shim(&setup).join(VERSION);
        fs::create_dir_all(version_dir.join(".windows-x86_64.123-456.staging")).expect("leftover");
        fs::write(
            version_dir.join(".windows-x86_64.123-456.staging/envoy.exe"),
            "half",
        )
        .expect("half");
        fs::create_dir_all(version_dir.join(".linux-x86_64-musl.1-2.staging")).expect("another");

        publish(&setup, &options(&setup)).expect("publishes");

        let left: Vec<String> = fs::read_dir(&version_dir)
            .expect("lists")
            .map(|item| {
                item.expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !left.iter().any(|name| name.starts_with(".windows-x86_64.")),
            "{left:?}"
        );
        assert!(
            left.contains(&".linux-x86_64-musl.1-2.staging".to_string()),
            "another platform's is not ours to clear"
        );
        assert!(version_dir.join("windows-x86_64/envoy.exe").is_file());
    }

    #[test]
    fn a_copy_that_does_not_read_back_is_not_put_in_place() {
        let setup = setup();
        release_of_all(&setup);
        let _fault = after_copy_do(|staging| {
            if staging.to_string_lossy().contains(".linux-x86_64-musl.") {
                fs::write(staging.join("envoy"), "damaged").expect("damage is done");
            }
        });

        let error = message(publish(&setup, &options(&setup)));

        assert!(
            error.contains("does not read back as it was written"),
            "{error}"
        );
        let version_dir = shim(&setup).join(VERSION);
        assert!(!version_dir.join("linux-x86_64-musl").exists());
        let left: Vec<String> = fs::read_dir(&version_dir)
            .expect("lists")
            .map(|item| {
                item.expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !left.iter().any(|name| name.ends_with(".staging")),
            "{left:?}"
        );
        assert!(
            !shim(&setup).join(POINTER_FILE).exists(),
            "the pointer is not moved"
        );
        assert!(
            !shim(&setup).join("install-envoy.ps1").exists(),
            "nor are the installers"
        );
    }

    #[test]
    fn a_platform_that_was_not_placed_is_placed_by_publishing_again() {
        let setup = setup();
        release_of_all(&setup);
        {
            let _fault = after_copy_do(|staging| {
                if staging.to_string_lossy().contains(".macos-x86_64.") {
                    fs::write(staging.join("envoy"), "damaged").expect("damage is done");
                }
            });
            publish(&setup, &options(&setup)).expect_err("the last platform fails");
        }

        let report = publish(&setup, &options(&setup)).expect("the rest is done");

        assert_eq!(report.outcome, ShimOutcome::Published);
        let placed: Vec<Target> = report
            .launchers
            .iter()
            .filter(|launcher| launcher.placing == Placing::Placed)
            .map(|launcher| launcher.target)
            .collect();
        assert!(placed.contains(&Target::MacosX64), "{placed:?}");
        assert!(report.missing.is_empty());
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn the_installers_are_replaced_only_when_they_differ() {
        let setup = setup();
        release_of_all(&setup);
        publish(&setup, &options(&setup)).expect("publishes");
        fs::write(shim(&setup).join("install-envoy.ps1"), "an older installer").expect("older");

        let report = publish(&setup, &options(&setup)).expect("publishes again");

        let installers = report
            .installers
            .clone()
            .expect("installers move with the pointer");
        assert_eq!(installers[0], ("install-envoy.ps1", Copying::Copied));
        assert!(installers[1..]
            .iter()
            .all(|(_, copying)| *copying == Copying::Same));
        assert_eq!(report.outcome, ShimOutcome::Published);
        assert_eq!(
            fs::read(shim(&setup).join("install-envoy.ps1")).expect("reads"),
            b"windows installer"
        );
    }

    #[test]
    fn only_the_installers_of_the_platforms_published_are_copied() {
        let setup = setup();
        add_release(&setup, VERSION, &[Target::WindowsX64], "build 1");

        let report = publish(&setup, &options(&setup)).expect("publishes");

        let names: Vec<&str> = report
            .installers
            .expect("installers move with the pointer")
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(names, ["install-envoy.ps1", "uninstall-envoy.ps1"]);
        assert!(!shim(&setup).join("install-envoy.sh").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_program_keeps_its_mode_and_everything_is_readable_by_all() {
        use std::os::unix::fs::PermissionsExt;

        let setup = setup();
        release_of_all(&setup);

        publish(&setup, &options(&setup)).expect("publishes");

        let mode =
            |path: PathBuf| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        let linux = shim(&setup).join(VERSION).join("linux-x86_64-musl");
        assert_eq!(mode(linux.join("envoy")), 0o755);
        assert_eq!(mode(linux.join("LICENSE")), 0o644);
        let windows = shim(&setup).join(VERSION).join("windows-x86_64");
        assert_eq!(mode(windows.join("envoy.exe")) & 0o044, 0o044);
        assert_eq!(mode(shim(&setup).join(POINTER_FILE)), 0o644);
        assert_eq!(mode(shim(&setup).join("install-envoy.sh")), 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn a_filesystem_that_refuses_to_change_modes_can_still_be_published_to() {
        let setup = setup();
        release_of_all(&setup);
        let _refuse = crate::share::faults::refuse_modes();

        let report = publish(&setup, &options(&setup)).expect("publishes anyway");

        assert_eq!(report.outcome, ShimOutcome::Published);
        assert!(shim(&setup)
            .join(VERSION)
            .join("linux-x86_64-musl/envoy")
            .is_file());
        assert_eq!(pointer_text(&setup), pointer_bytes(VERSION));
    }

    #[test]
    fn the_report_lists_what_was_done_in_plain_words() {
        let setup = setup();
        add_release(
            &setup,
            VERSION,
            &[Target::WindowsX64, Target::MacosX64],
            "build 1",
        );
        let run = |_: &Path| -> io::Result<String> { Ok("envoy-shim v0.8.0".to_string()) };

        let report = publish_with(
            &options(&setup),
            &|_| None,
            &hooks(Some(Target::WindowsX64), &run),
        )
        .expect("publishes");

        let text = report.to_string();
        assert!(
            text.starts_with("Published the launcher v0.8.0.\n"),
            "{text}"
        );
        assert!(text.contains("windows-x86_64"), "{text}");
        assert!(text.contains("placed (6 files"), "{text}");
        assert!(text.contains("ran and reports v0.8.0"), "{text}");
        assert!(
            text.contains("None on the share for: linux-x86_64-musl, macos-aarch64"),
            "{text}"
        );
        assert!(text.contains("install-envoy.ps1"), "{text}");
        assert!(text.contains("current.json now names v0.8.0"), "{text}");
    }
}
