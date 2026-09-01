//! Error hierarchy for `engit-core`, ported from `py/engit/_exceptions.py`.
//!
//! Python models these as exception subclasses. Rust represents them as a
//! single enum plus a convenience `Result<T>` alias.

use std::path::PathBuf;

use thiserror::Error;

/// Convenience alias used throughout `engit-core`.
pub type Result<T> = std::result::Result<T, EngitError>;

/// Root error type for all fallible `engit-core` operations.
#[derive(Debug, Error)]
pub enum EngitError {
    /// Generic root error mirroring Python's `EngitError`.
    #[error("{0}")]
    Engit(String),

    /// Corresponds to Python's `GitError`.
    #[error("{0}")]
    Git(String),

    /// Corresponds to Python's `NotAGitRepoError`.
    #[error(
        "Not inside a git repository. Run engit commands from within a git \
working directory."
    )]
    NotAGitRepo,

    /// Corresponds to Python's `NoTagsFoundError`.
    #[error("{0}")]
    NoTagsFound(String),

    /// Corresponds to Python's `SemVerError`.
    #[error("{0}")]
    SemVer(String),

    /// Corresponds to Python's `GitHubError`.
    #[error("{0}")]
    GitHub(String),

    /// Corresponds to Python's `GhCliNotFoundError`.
    #[error(
        "'gh' CLI not found on PATH. Install it from \
https://cli.github.com/ to use this command."
    )]
    GhCliNotFound,

    /// Local module-specific error for publish flows.
    #[error("{0}")]
    Publish(String),

    /// Local module-specific error for bundle cache maintenance flows.
    #[error("{0}")]
    Cache(String),

    /// Validation / usage error.
    #[error("{0}")]
    Validation(String),

    /// Failure reported by the Envoy framework contract.
    #[error("{0}")]
    Framework(String),

    /// I/O error with path context.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// JSON parse error with path context.
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// YAML parse error with path context.
    #[error("invalid YAML in {path}: {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
}

impl EngitError {
    /// Construct a git failure from captured command output.
    pub fn git_command(args: &[String], stdout: &str, stderr: &str) -> Self {
        EngitError::Git(format_command_failure("git", args, stdout, stderr))
    }

    /// Construct a GitHub CLI failure from captured command output.
    pub fn github_command(args: &[String], stdout: &str, stderr: &str) -> Self {
        EngitError::GitHub(format_command_failure("gh", args, stdout, stderr))
    }

    /// Construct an I/O error with path context.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        EngitError::Io {
            path: path.into(),
            source,
        }
    }

    /// Construct a JSON parse error with path context.
    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        EngitError::Json {
            path: path.into(),
            source,
        }
    }

    /// Construct a YAML parse error with path context.
    pub fn yaml(path: impl Into<PathBuf>, source: serde_yaml::Error) -> Self {
        EngitError::Yaml {
            path: path.into(),
            source,
        }
    }
}

fn format_command_failure(tool: &str, args: &[String], stdout: &str, stderr: &str) -> String {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    let fallback = format!("{tool} {} failed.", args.join(" "));

    match (stderr.is_empty(), stdout.is_empty()) {
        (false, true) => stderr.to_string(),
        (false, false) => format!("{stderr}\nstdout:\n{stdout}"),
        (true, false) => stdout.to_string(),
        (true, true) => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::format_command_failure;

    #[test]
    fn command_failure_prefers_stderr() {
        let args = vec![String::from("status")];
        let message = format_command_failure("git", &args, "ok", "fatal: bad");

        assert_eq!(message, "fatal: bad\nstdout:\nok");
    }

    #[test]
    fn command_failure_falls_back_to_stdout() {
        let args = vec![String::from("status")];
        let message = format_command_failure("git", &args, "problem", "");

        assert_eq!(message, "problem");
    }

    #[test]
    fn command_failure_uses_generic_message_when_empty() {
        let args = vec![String::from("status")];
        let message = format_command_failure("git", &args, "", "");

        assert_eq!(message, "git status failed.");
    }
}
