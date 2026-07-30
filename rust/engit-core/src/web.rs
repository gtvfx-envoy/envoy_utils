//! Open the current repository on GitHub.

use std::path::Path;
use std::process::Command;

use crate::error::{EngitError, Result};
use crate::git::{get_current_branch, get_remote_url, require_git_repo};

pub(crate) fn to_https_url(remote_url: &str) -> String {
    let mut url = remote_url.trim().to_string();

    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            url = format!("https://{host}/{path}");
        }
    }

    if url.ends_with(".git") {
        url.truncate(url.len() - 4);
    }

    url
}

fn open_url(url: &str) -> Result<()> {
    let status = if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).status()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    }
    .map_err(|source| EngitError::Git(format!("Could not open web browser: {source}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(EngitError::Git(format!(
            "Could not open web browser: command exited with status {status}."
        )))
    }
}

/// Open the current repository on GitHub in a browser.
pub fn run_web(branch: Option<&str>, remote: &str, cwd: Option<&Path>) -> Result<()> {
    require_git_repo(cwd)?;

    let raw_url = get_remote_url(remote, cwd).ok_or_else(|| {
        EngitError::Git(format!(
            "Remote '{remote}' has no URL. Check your git remote configuration."
        ))
    })?;
    let base_url = to_https_url(&raw_url);
    let resolved_branch = branch
        .map(ToOwned::to_owned)
        .or_else(|| get_current_branch(cwd));
    let url = resolved_branch
        .map(|branch| format!("{base_url}/tree/{branch}"))
        .unwrap_or(base_url);

    println!("Opening {url}");
    open_url(&url)
}

#[cfg(test)]
mod tests {
    use super::to_https_url;

    #[test]
    fn normalizes_ssh_and_https_urls() {
        assert_eq!(
            to_https_url("git@github.com:owner/repo.git"),
            "https://github.com/owner/repo"
        );
        assert_eq!(
            to_https_url("https://github.com/owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }
}
