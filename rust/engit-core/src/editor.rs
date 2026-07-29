//! Shared editor helper for interactive engit flows.
//!
//! Unlike the Python implementation, scratch files are created under the
//! current working directory rather than the system temp directory so that the
//! crate can run in locked-down environments that disallow temp paths.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{EngitError, Result};

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static EDITOR_ENV_MUTEX: Mutex<()> = Mutex::new(());

fn split_command(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in value.chars() {
        match quote {
            Some(active) if ch == active => {
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Return the editor command as a tokenized command line.
pub fn find_editor() -> Vec<String> {
    for variable in ["GIT_EDITOR", "VISUAL", "EDITOR"] {
        if let Ok(value) = env::var(variable) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return split_command(trimmed);
            }
        }
    }

    if cfg!(windows) {
        vec![String::from(r"C:\Windows\notepad.exe")]
    } else {
        vec![String::from("vim")]
    }
}

fn scratch_root() -> Result<PathBuf> {
    let cwd = env::current_dir().map_err(|source| EngitError::Git(source.to_string()))?;
    let scratch_root = cwd.join(".engit-edit");

    fs::create_dir_all(&scratch_root)
        .map_err(|source| EngitError::io(scratch_root.clone(), source))?;

    Ok(scratch_root)
}

fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|ch| {
            if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

fn unique_scratch_path(filename: &str) -> Result<PathBuf> {
    let root = scratch_root()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sanitized = sanitize_filename(filename);

    Ok(root.join(format!("{timestamp}-{}-{sanitized}", std::process::id())))
}

fn remove_if_empty(path: &Path) {
    if path.is_dir()
        && path
            .read_dir()
            .map(|mut items| items.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(path);
    }
}

/// Open `content` in the user's editor and return the edited text.
pub fn open_in_editor(content: &str, filename: &str) -> Result<Option<String>> {
    let command = find_editor();
    let editor_name = command.join(" ");
    let scratch_path = unique_scratch_path(filename)?;
    fs::write(&scratch_path, content)
        .map_err(|source| EngitError::io(scratch_path.clone(), source))?;

    let metadata = fs::metadata(&scratch_path)
        .map_err(|source| EngitError::io(scratch_path.clone(), source))?;
    let modified_before = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    println!(
        "Opening editor ({editor_name}) — save the file to confirm, close without \
saving to cancel."
    );

    let status = Command::new(&command[0])
        .args(&command[1..])
        .arg(&scratch_path)
        .status()
        .map_err(|source| {
            EngitError::Engit(format!("Failed to launch editor '{editor_name}': {source}"))
        })?;

    if !status.success() {
        let _ = fs::remove_file(&scratch_path);
        if let Some(parent) = scratch_path.parent() {
            remove_if_empty(parent);
        }
        return Err(EngitError::Engit(format!(
            "Editor '{editor_name}' exited with status {status}."
        )));
    }

    let metadata = fs::metadata(&scratch_path)
        .map_err(|source| EngitError::io(scratch_path.clone(), source))?;
    let modified_after = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let contents = fs::read_to_string(&scratch_path)
        .map_err(|source| EngitError::io(scratch_path.clone(), source))?;

    fs::remove_file(&scratch_path).ok();
    if let Some(parent) = scratch_path.parent() {
        remove_if_empty(parent);
    }

    if modified_after == modified_before {
        Ok(None)
    } else {
        Ok(Some(contents))
    }
}

#[cfg(test)]
mod tests {
    use super::{find_editor, split_command, EDITOR_ENV_MUTEX};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
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

    #[test]
    fn split_command_respects_quotes() {
        assert_eq!(
            split_command(r#""C:\Program Files\Code\Code.exe" --wait"#),
            vec![
                String::from(r"C:\Program Files\Code\Code.exe"),
                String::from("--wait"),
            ]
        );
    }

    #[test]
    fn find_editor_uses_git_editor_priority() {
        let _lock = EDITOR_ENV_MUTEX.lock().expect("editor env mutex poisoned");
        let _git_editor = EnvVarGuard::set("GIT_EDITOR", Some("code --wait"));
        let _visual = EnvVarGuard::set("VISUAL", Some("vim"));
        let _editor = EnvVarGuard::set("EDITOR", Some("nano"));

        assert_eq!(
            find_editor(),
            vec![String::from("code"), String::from("--wait")]
        );
    }
}
