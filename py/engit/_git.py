"""Low-level git subprocess wrappers.

All functions call ``git`` via :mod:`subprocess` and raise typed exceptions
rather than letting raw :class:`subprocess.CalledProcessError` propagate.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from ._exceptions import GitError, NotAGitRepoError, NoTagsFoundError
from ._semver import SemVer


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _run(*args: str, cwd: Path | None = None) -> str:
    """Run a git command and return stripped stdout.

    Args:
        *args: Arguments passed to ``git`` (excluding the ``git`` prefix).
        cwd: Working directory. Defaults to the current directory.

    Returns:
        Stripped stdout string.

    Raises:
        ~._exceptions.GitError: If the command exits with a non-zero code.

    """
    cmd = ['git', *args]
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            cwd=cwd,
        )
    except FileNotFoundError:
        raise GitError("'git' executable not found on PATH.")

    if result.returncode != 0:
        raise GitError(result.stderr.strip() or f"git {' '.join(args)} failed.")

    return result.stdout.strip()


# ---------------------------------------------------------------------------
# Repository inspection
# ---------------------------------------------------------------------------

def is_git_repo(cwd: Path | None = None) -> bool:
    """Return ``True`` if *cwd* (or the current directory) is inside a git repo."""
    try:
        _run('rev-parse', '--git-dir', cwd=cwd)
        return True
    except GitError:
        return False


def require_git_repo(cwd: Path | None = None) -> None:
    """Raise :class:`~._exceptions.NotAGitRepoError` if not inside a git repo.

    Args:
        cwd: Directory to check. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If the directory is not a git repo.

    """
    if not is_git_repo(cwd):
        raise NotAGitRepoError(
            "Not inside a git repository. "
            "Run engit commands from within a git working directory."
        )


def get_repo_root(cwd: Path | None = None) -> Path:
    """Return the absolute path to the root of the git repository.

    Args:
        cwd: Starting directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If the directory is not a git repo.

    """
    require_git_repo(cwd)
    return Path(_run('rev-parse', '--show-toplevel', cwd=cwd))


def get_remote_url(remote: str = 'origin', cwd: Path | None = None) -> str | None:
    """Return the URL for *remote*, or ``None`` if the remote does not exist."""
    try:
        return _run('remote', 'get-url', remote, cwd=cwd)
    except GitError:
        return None


# ---------------------------------------------------------------------------
# Tag operations
# ---------------------------------------------------------------------------

def get_latest_tag(cwd: Path | None = None) -> str | None:
    """Return the most recent tag reachable from HEAD, or ``None``.

    Uses ``git describe --tags --abbrev=0`` which finds the nearest ancestor tag.

    Args:
        cwd: Working directory. Defaults to the current directory.

    """
    try:
        return _run('describe', '--tags', '--abbrev=0', cwd=cwd)
    except GitError:
        return None


def get_latest_semver_tag(cwd: Path | None = None) -> SemVer | None:
    """Return the most recent tag that is a valid semver, or ``None``.

    Iterates all tags sorted by version (newest first) and returns the first
    one that parses as a :class:`~._semver.SemVer`.

    Args:
        cwd: Working directory. Defaults to the current directory.

    """
    from ._semver import SemVerError  # local to avoid circular

    try:
        raw = _run(
            'tag',
            '--list',
            '--sort=-version:refname',
            cwd=cwd,
        )
    except GitError:
        return None

    for line in raw.splitlines():
        tag = line.strip()
        if not tag:
            continue
        try:
            return SemVer.parse(tag)
        except SemVerError:
            continue

    return None


def create_tag(tag: str, message: str, cwd: Path | None = None) -> None:
    """Create an annotated git tag at HEAD.

    Args:
        tag: Tag name (e.g. ``'v1.2.3'``).
        message: Annotation message shown by ``git tag -v``.
        cwd: Working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.GitError: If tag creation fails.

    """
    _run('tag', '-a', tag, '-m', message, cwd=cwd)


def push_tag(tag: str, remote: str = 'origin', cwd: Path | None = None) -> None:
    """Push a single tag to *remote*.

    Args:
        tag: Tag name to push.
        remote: Remote name. Defaults to ``'origin'``.
        cwd: Working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.GitError: If the push fails.

    """
    _run('push', remote, tag, cwd=cwd)


# ---------------------------------------------------------------------------
# Commit history
# ---------------------------------------------------------------------------

def get_commits_since(ref: str, cwd: Path | None = None) -> list[str]:
    """Return commit subject lines between *ref* and HEAD.

    Args:
        ref: A tag name, SHA, or any git ref to start from (exclusive).
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A list of one-line commit messages, most recent first.

    """
    try:
        raw = _run(
            'log',
            f'{ref}..HEAD',
            '--pretty=format:%s',
            '--no-merges',
            cwd=cwd,
        )
    except GitError:
        return []

    return [line for line in raw.splitlines() if line.strip()]


def get_all_commits(cwd: Path | None = None) -> list[str]:
    """Return all commit subject lines from the beginning of history.

    Used when there are no prior tags to reference.

    Args:
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A list of one-line commit messages, most recent first.

    """
    try:
        raw = _run(
            'log',
            '--pretty=format:%s',
            '--no-merges',
            cwd=cwd,
        )
    except GitError:
        return []

    return [line for line in raw.splitlines() if line.strip()]
