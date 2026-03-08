"""engit cleanup command — tidy up merged and stale local branches.

Performs the following cleanup steps:

* Runs ``git remote prune <remote>`` to remove stale remote-tracking refs.
* Deletes local branches that have been merged into the current branch.
* Deletes local branches whose remote-tracking branch no longer exists.

The current branch and remote default branches (``main``, ``master``,
``develop``) are always protected.
"""

from __future__ import annotations

from pathlib import Path

from ._exceptions import GitError
from ._git import (
    require_git_repo,
    get_current_branch,
    get_merged_branches,
    get_remote_tracking_branch,
    delete_local_branch,
    prune_remote,
    _run,
)

# Branch names that should never be force-deleted by cleanup.
_PROTECTED = frozenset({'main', 'master', 'develop'})


def run_cleanup(
    *,
    remote: str = 'origin',
    noop: bool = False,
    cwd: Path | None = None,
) -> None:
    """Clean up merged and stale local branches.

    Args:
        remote: Remote name to prune and check. Defaults to ``'origin'``.
        noop: When ``True``, print what would happen without deleting anything.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.

    """
    require_git_repo(cwd=cwd)

    current = get_current_branch(cwd=cwd) or ''

    # ---- Prune stale remote-tracking refs ----
    try:
        if noop:
            _run('remote', 'prune', remote, '--dry-run', cwd=cwd)
        else:
            prune_remote(remote, cwd=cwd)
            print(f'Pruned stale remote-tracking refs for {remote}.')
    except GitError as exc:
        print(f'Warning: could not prune remote "{remote}": {exc}')

    deleted: list[str] = []
    skipped: list[str] = []

    # ---- Delete merged branches ----
    for branch in get_merged_branches(cwd=cwd):
        if branch == current or branch in _PROTECTED:
            continue
        if noop:
            print(f'  Would delete {branch} [merged]')
            continue
        try:
            delete_local_branch(branch, force=False, cwd=cwd)
            deleted.append(branch)
            print(f'  Deleted {branch} [merged]')
        except GitError as exc:
            skipped.append(branch)
            print(f'  Skipped {branch}: {exc}')

    # ---- Delete branches whose remote is gone ----
    try:
        raw = _run('branch', cwd=cwd)
    except GitError:
        raw = ''

    for line in raw.splitlines():
        branch = line.lstrip('* ').strip()
        if not branch or branch == current or branch in _PROTECTED:
            continue
        if branch in deleted:
            continue

        tracking = get_remote_tracking_branch(branch, cwd=cwd)
        if tracking is None:
            continue  # No upstream set; leave it alone.

        # Check whether the remote branch still exists
        try:
            _run('show-ref', '--verify', '--quiet', f'refs/remotes/{tracking}', cwd=cwd)
            # Still exists — skip
        except GitError:
            # Remote branch is gone
            if noop:
                print(f'  Would delete {branch} [remote deleted]')
                continue
            try:
                delete_local_branch(branch, force=True, cwd=cwd)
                deleted.append(branch)
                print(f'  Deleted {branch} [remote deleted]')
            except GitError as exc:
                skipped.append(branch)
                print(f'  Skipped {branch}: {exc}')

    if not noop:
        if deleted:
            print(f'\nDeleted {len(deleted)} branch(es).')
            print('Use `git branch <name> <revision>` or `git reflog` to restore if needed.')
        else:
            print('No local branches to clean up.')
        if skipped:
            print(f'Warning: {len(skipped)} branch(es) could not be deleted: {", ".join(skipped)}')
