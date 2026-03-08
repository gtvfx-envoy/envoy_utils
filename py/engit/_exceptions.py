"""engit exception hierarchy."""

from __future__ import annotations


class EngitError(Exception):
    """Base class for all engit errors."""


class GitError(EngitError):
    """Raised when a git operation fails."""


class NotAGitRepoError(GitError):
    """Raised when the current directory is not inside a git repository."""


class NoTagsFoundError(GitError):
    """Raised when no version tags exist in the repository."""


class SemVerError(EngitError):
    """Raised when a version string fails semantic version validation."""


class GitHubError(EngitError):
    """Raised when a GitHub operation fails."""


class GhCliNotFoundError(GitHubError):
    """Raised when the ``gh`` CLI executable is not found on PATH."""
