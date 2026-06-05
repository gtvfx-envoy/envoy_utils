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


def get_sorted_semver_tags(cwd: Path | None = None) -> list[str]:
    """Return all semver tags sorted newest-first as raw tag strings.

    Only tags that parse as valid :class:`~._semver.SemVer` are included.
    Non-semver tags (e.g. feature branches, date-based tags) are silently
    discarded.

    Args:
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A list of tag strings such as ``['v1.2.0', 'v1.1.0', 'v0.9.0']``.

    """
    from ._semver import SemVerError

    try:
        raw = _run('tag', '--list', '--sort=-version:refname', cwd=cwd)
    except GitError:
        return []

    tags = []
    for line in raw.splitlines():
        tag = line.strip()
        if not tag:
            continue
        try:
            SemVer.parse(tag)
            tags.append(tag)
        except SemVerError:
            continue

    return tags


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


def get_tag_annotation(tag: str, cwd: Path | None = None) -> str | None:
    """Return the annotation message for an annotated tag.

    Args:
        tag: Tag name to inspect (e.g. ``'v1.2.3'``).
        cwd: Working directory. Defaults to the current directory.

    Returns:
        The tag annotation body, or ``None`` when the tag does not exist,
        is lightweight (non-annotated), or has no message body.

    """
    try:
        object_type = _run('cat-file', '-t', tag, cwd=cwd)
    except GitError:
        return None

    if object_type != 'tag':
        # Lightweight tags resolve to commit objects and have no annotation.
        return None

    try:
        body = _run('for-each-ref', f'refs/tags/{tag}', '--format=%(contents)', cwd=cwd)
    except GitError:
        return None

    body = body.strip()
    return body or None


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


def push_branch_and_tag(tag: str, branch: str, remote: str = 'origin', cwd: Path | None = None) -> None:
    """Push a branch and a tag together in one operation.

    Equivalent to ``git push <remote> <branch> <tag>``.

    Args:
        tag: Tag name to push.
        branch: Branch name to push.
        remote: Remote name. Defaults to ``'origin'``.
        cwd: Working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.GitError: If the push fails.

    """
    _run('push', remote, branch, tag, cwd=cwd)


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


# ---------------------------------------------------------------------------
# Branch helpers
# ---------------------------------------------------------------------------

def get_current_branch(cwd: Path | None = None) -> str | None:
    """Return the name of the currently checked-out branch, or ``None`` if detached.

    Args:
        cwd: Working directory. Defaults to the current directory.

    """
    try:
        result = _run('rev-parse', '--abbrev-ref', 'HEAD', cwd=cwd)
        return None if result == 'HEAD' else result
    except GitError:
        return None


def get_branch_comparison(
    branch: str,
    remote: str = 'origin',
    cwd: Path | None = None,
) -> str:
    """Return a human-readable ahead/behind summary for *branch* vs its upstream.

    Returns an empty string if the branch has no upstream or the comparison
    cannot be determined.

    Args:
        branch: Local branch name.
        remote: Remote name. Defaults to ``'origin'``.
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A string such as ``'ahead 2'``, ``'behind 1'``, ``'ahead 1, behind 3'``,
        or ``''`` when in sync or unknown.

    """
    upstream = f'{remote}/{branch}'
    try:
        raw = _run(
            'rev-list',
            '--left-right',
            '--count',
            f'{upstream}...HEAD',
            cwd=cwd,
        )
    except GitError:
        return ''

    parts = raw.split()
    if len(parts) != 2:
        return ''
    behind, ahead = int(parts[0]), int(parts[1])
    pieces = []
    if ahead:
        pieces.append(f'ahead {ahead}')
    if behind:
        pieces.append(f'behind {behind}')
    return ', '.join(pieces)


def get_last_commit_summary(cwd: Path | None = None) -> str:
    """Return the subject and short SHA of the most recent commit.

    Args:
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A string like ``'abc1234 Fix the thing'``, or ``''`` if history is empty.

    """
    try:
        return _run('log', '--oneline', '-1', cwd=cwd)
    except GitError:
        return ''


# ---------------------------------------------------------------------------
# Tag existence
# ---------------------------------------------------------------------------

def tag_exists(tag: str, cwd: Path | None = None) -> bool:
    """Return ``True`` if *tag* exists in the local repository.

    Args:
        tag: Tag name to check.
        cwd: Working directory. Defaults to the current directory.

    """
    try:
        result = _run('tag', '--list', tag, cwd=cwd)
        return bool(result.strip())
    except GitError:
        return False


def pull(
    remote: str = 'origin',
    rebase: bool = False,
    cwd: Path | None = None,
) -> str:
    """Run ``git pull`` and return the output message.

    Args:
        remote: Remote name to pull from. Defaults to ``'origin'``.
        rebase: Pass ``--rebase`` to rebase local commits on top of the
            fetched branch instead of merging.
        cwd: Working directory (git repo root). Defaults to the current
            directory.

    Returns:
        Git output string (e.g. ``'Already up to date.'`` or a summary of
        files changed).

    Raises:
        ~._exceptions.GitError: If the pull fails.

    """
    args = ['pull']
    if rebase:
        args.append('--rebase')
    args.append(remote)
    return _run(*args, cwd=cwd)


# ---------------------------------------------------------------------------
# Branch merging / cleanup helpers
# ---------------------------------------------------------------------------

def get_merged_branches(cwd: Path | None = None) -> list[str]:
    """Return local branches that have been merged into the current branch.

    The current branch itself (prefixed with ``*``) is excluded.

    Args:
        cwd: Working directory. Defaults to the current directory.

    Returns:
        A list of branch name strings.

    """
    try:
        raw = _run('branch', '--merged', cwd=cwd)
    except GitError:
        return []
    branches = []
    for line in raw.splitlines():
        name = line.lstrip('* ').strip()
        if name:
            branches.append(name)
    return branches


def delete_local_branch(branch: str, force: bool = False, cwd: Path | None = None) -> None:
    """Delete a local branch.

    Args:
        branch: Branch name to delete.
        force: Use ``-D`` (force-delete) instead of ``-d``.
        cwd: Working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.GitError: If deletion fails.

    """
    flag = '-D' if force else '-d'
    _run('branch', flag, branch, cwd=cwd)


def prune_remote(remote: str = 'origin', cwd: Path | None = None) -> None:
    """Run ``git remote prune`` to remove stale remote-tracking refs.

    Args:
        remote: Remote name. Defaults to ``'origin'``.
        cwd: Working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.GitError: If the prune fails.

    """
    _run('remote', 'prune', remote, cwd=cwd)


def get_remote_tracking_branch(branch: str, cwd: Path | None = None) -> str | None:
    """Return the remote-tracking ref for *branch*, or ``None`` if unset.

    Args:
        branch: Local branch name.
        cwd: Working directory. Defaults to the current directory.

    """
    try:
        remote = _run('config', f'branch.{branch}.remote', cwd=cwd)
        merge = _run('config', f'branch.{branch}.merge', cwd=cwd)
    except GitError:
        return None
    if merge.startswith('refs/heads/'):
        merge = merge[len('refs/heads/'):]
    return f'{remote}/{merge}'
