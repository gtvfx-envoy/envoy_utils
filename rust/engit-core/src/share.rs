//! Writing into a folder that people publish into and launchers read.
//!
//! What publishing the redirect file and publishing the launcher have in common:
//! the config root they write under, the turn publishers take on a folder, how a
//! new folder is opened to the people who need it, and how a file is put in
//! place so that a reader sees the old one or the new one and never half of
//! either.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::error::{EngitError, Result};

/// The config root `engit` publishes into.
///
/// The launcher reads `ENVOY_CONFIG_PROD` instead. It is deliberately not used
/// to publish, so that a consumer's environment cannot publish by accident.
pub const CONFIG_PUBLISH_ROOT_VAR: &str = "ENVOY_CONFIG_PUBLISH_ROOT";

/// How long a publish waits for another one into the same folder to finish.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(10);

pub(crate) type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub(crate) fn setting(env: &Env, name: &str) -> Option<String> {
    env(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn publish_root(output: Option<&Path>, env: &Env) -> Result<PathBuf> {
    if let Some(path) = output {
        return Ok(path.to_path_buf());
    }
    setting(env, CONFIG_PUBLISH_ROOT_VAR)
        .map(PathBuf::from)
        .ok_or_else(|| {
            EngitError::Publish(format!(
                "No --output specified and {CONFIG_PUBLISH_ROOT_VAR} is not set."
            ))
        })
}

/// A mistyped root must not become a new folder that no launcher reads.
pub(crate) fn require_folder(root: &Path) -> Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    Err(EngitError::Publish(format!(
        "{} is not a folder. The config root must already exist: check the path.",
        root.display()
    )))
}

pub(crate) fn confirm_written(live: &Path, bytes: &[u8]) -> Result<()> {
    match fs::read(live) {
        Ok(found) if found == bytes => Ok(()),
        Ok(_) => Err(EngitError::Publish(format!(
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
pub(crate) struct PublishLock {
    file: File,
    /// Set when the folder cannot lock files, so there was nothing to wait for.
    pub(crate) note: Option<String>,
}

impl PublishLock {
    pub(crate) fn acquire(path: &Path, wait: Duration) -> Result<Self> {
        let file = open_lock_file(path)?;
        let give_up = Instant::now() + wait;
        loop {
            match try_lock(&file) {
                Ok(()) => return Ok(Self { file, note: None }),
                Err(TryLockError::WouldBlock) if Instant::now() < give_up => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(TryLockError::WouldBlock) => {
                    let folder = path.parent().unwrap_or(path);
                    return Err(EngitError::Publish(format!(
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

/// Try to take the lock on a file, without waiting.
fn try_lock(file: &File) -> std::result::Result<(), TryLockError> {
    #[cfg(test)]
    if faults::locking_is_unsupported() {
        return Err(TryLockError::Error(io::Error::from(
            io::ErrorKind::Unsupported,
        )));
    }
    file.try_lock()
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
pub(crate) fn create_folder(path: &Path) -> Result<()> {
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
/// When it is in the group of the folder it is in, it takes that group's access
/// and the group bit too, so that the administrators who can publish there can
/// publish here. It takes nothing of what the parent gives everyone else, and
/// not its sticky bit: a world-writable root must not make a world-writable
/// folder.
#[cfg(any(unix, test))]
fn new_folder_mode(parent_mode: u32, in_parents_group: bool) -> u32 {
    let group = if in_parents_group {
        parent_mode & 0o2070
    } else {
        0
    };
    0o755 | group
}

/// Give a new folder the access of the folder it is in, as [`new_folder_mode`] says.
///
/// What the parent gives its group means something here only if the new folder
/// is in that group. It is on macOS, and on Linux when the parent has the group
/// bit. Otherwise the new folder takes the publisher's own group, which can be a
/// broad one, and copying the parent's group access would hand that group the
/// folder. So it is handed to the parent's group when the publisher is a member
/// of it, and where they are not it keeps its group and the group gets nothing.
#[cfg(unix)]
fn match_parent_access(path: &Path) {
    use std::os::unix::fs::chown;

    match_parent_access_with(path, |folder, group| chown(folder, None, Some(group)));
}

/// [`match_parent_access`], with the way a folder is handed to a group passed in.
#[cfg(unix)]
fn match_parent_access_with(path: &Path, hand_to: impl FnOnce(&Path, u32) -> io::Result<()>) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Some(parent) = path.parent() else { return };
    let (Ok(parent), Ok(own)) = (fs::metadata(parent), fs::metadata(path)) else {
        return;
    };

    // Some filesystems report a change of group as done and change nothing (a
    // network mount that keeps no owners), so look at the group afterwards.
    let in_parents_group = own.gid() == parent.gid()
        || (hand_to(path, parent.gid()).is_ok()
            && fs::metadata(path).is_ok_and(|now| now.gid() == parent.gid()));
    let mode = new_folder_mode(parent.permissions().mode(), in_parents_group);
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
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
pub(crate) fn make_readable_by_all(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    #[cfg(test)]
    if faults::modes_are_refused() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    file.set_permissions(fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
pub(crate) fn make_readable_by_all(_file: &File) -> io::Result<()> {
    Ok(())
}

/// What to tell the publisher when the live file is not readable by everyone.
///
/// A filesystem that refuses to set modes leaves the file as the publisher's
/// umask made it, and with a strict one the launcher of every other user cannot
/// read it, while the publisher's own read-back passes. A network mount can also
/// show a mode of its own that is not what the server allows, so the note asks
/// the publisher to check, and offers the command only where modes are kept.
#[cfg(unix)]
pub(crate) fn unreadable_by_others(live: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(live).ok()?.permissions().mode();
    (mode & 0o044 != 0o044).then(|| {
        format!(
            "{} has mode {:o}, so the launchers of other users may not be able to read it. \
Check that they can; on a disk that keeps modes, chmod 644 {} makes it readable.",
            live.display(),
            mode & 0o777,
            live.display()
        )
    })
}

#[cfg(not(unix))]
pub(crate) fn unreadable_by_others(_live: &Path) -> Option<String> {
    None
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

/// Write `bytes` beside `live` and move them over it.
pub(crate) fn replace_file(directory: &Path, live: &Path, bytes: &[u8]) -> Result<()> {
    let unique = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let name = live
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
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
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

/// What a test makes go wrong in this module, on the thread it runs on.
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::Cell;

    thread_local! {
        #[cfg(unix)]
        static REFUSE_MODES: Cell<bool> = const { Cell::new(false) };
        static UNLOCKABLE: Cell<bool> = const { Cell::new(false) };
    }

    /// `true` while a test has made the filesystem refuse to change modes.
    #[cfg(unix)]
    pub(crate) fn modes_are_refused() -> bool {
        REFUSE_MODES.with(Cell::get)
    }

    /// `true` while a test has made the folder unable to lock files.
    pub(crate) fn locking_is_unsupported() -> bool {
        UNLOCKABLE.with(Cell::get)
    }

    /// Undoes a fault when it is dropped, even if the test panics.
    pub(crate) struct Fault(fn());

    impl Drop for Fault {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    /// The filesystem refuses to change modes until the guard is dropped.
    #[cfg(unix)]
    pub(crate) fn refuse_modes() -> Fault {
        REFUSE_MODES.with(|refuse| refuse.set(true));
        Fault(|| REFUSE_MODES.with(|refuse| refuse.set(false)))
    }

    /// The folder cannot lock files until the guard is dropped.
    pub(crate) fn unlockable() -> Fault {
        UNLOCKABLE.with(|unlockable| unlockable.set(true));
        Fault(|| UNLOCKABLE.with(|unlockable| unlockable.set(false)))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::create_folder;

    #[test]
    fn a_new_folder_has_the_group_access_of_its_parent_only_in_the_parents_group() {
        // The mode of a folder that is in the group of the folder it is in.
        assert_eq!(super::new_folder_mode(0o775, true), 0o775);
        assert_eq!(super::new_folder_mode(0o2775, true), 0o2775);
        assert_eq!(super::new_folder_mode(0o1777, true), 0o775);
        assert_eq!(super::new_folder_mode(0o777, true), 0o775);
        assert_eq!(super::new_folder_mode(0o750, true), 0o755);
        assert_eq!(super::new_folder_mode(0o700, true), 0o755);
        // In another group, what the parent gives its group is nothing to this one.
        for parent in [0o775, 0o2775, 0o1777, 0o777, 0o770] {
            assert_eq!(super::new_folder_mode(parent, false), 0o755, "{parent:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_hand_over_that_changes_nothing_is_not_taken_for_one() {
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // A folder in a shared temp folder, which belongs to root, so that the
        // folder and its parent are in different groups (unless the publisher is
        // root, and then there is nothing to show).
        let parent = Path::new("/tmp");
        let folder = parent.join(format!(
            "engit-redirect-lie-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        if !parent.is_dir() || fs::create_dir(&folder).is_err() {
            return;
        }
        let same_group = fs::metadata(&folder).expect("metadata").gid()
            == fs::metadata(parent).expect("metadata").gid();

        // A filesystem that says the group was changed and leaves it as it was.
        super::match_parent_access_with(&folder, |_, _| Ok(()));

        let mode = fs::metadata(&folder)
            .expect("metadata")
            .permissions()
            .mode();
        let _ = fs::remove_dir(&folder);
        if !same_group {
            assert_eq!(
                mode & 0o777,
                0o755,
                "{mode:o} took group access it has no right to"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_new_folder_in_another_group_does_not_get_that_groups_access() {
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // A shared temp folder belongs to root, not to the publisher's group
        // (unless the publisher is root, when the two are the same and the check
        // below holds trivially).
        let parent = Path::new("/tmp");
        if !parent.is_dir() {
            return;
        }
        let folder = parent.join(format!(
            "engit-redirect-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        create_folder(&folder).expect("the folder is created");

        let (child, parent) = (
            fs::metadata(&folder).expect("metadata"),
            fs::metadata(parent).expect("metadata"),
        );
        let _ = fs::remove_dir(&folder);
        let mode = child.permissions().mode();
        assert_eq!(
            mode & 0o1002,
            0,
            "{mode:o} is writable by everyone or sticky"
        );
        if mode & 0o020 != 0 {
            assert_eq!(
                child.gid(),
                parent.gid(),
                "{mode:o} gives group access to a group that is not the parent's"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_a_folder_that_is_already_there() {
        // Windows refuses to create a drive root with "access is denied", not "already exists".
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());

        create_folder(Path::new(&format!("{drive}\\"))).expect("a drive root is a folder");
    }
}
