"""engit status command — repository status summary.

Displays branch, ahead/behind the remote, last semver tag, and most recent
commit in a compact, human-readable format.
"""

from __future__ import annotations

from pathlib import Path

from ._git import (
    getBranchComparison,
    getCurrentBranch,
    getLastCommitSummary,
    getLatestSemverTag,
    requireGitRepo,
)


def runStatus(
    *,
    remote: str = 'origin',
    cwd: Path | None = None,
) -> None:
    """Print a one-screen status summary for the current repository.

    Shows:

    * Current branch name
    * Ahead / behind count vs the remote-tracking branch
    * Most recent semver tag (or ``(none)`` if untagged)
    * Subject line and short SHA of the latest commit

    Args:
        remote: Remote name used for ahead/behind comparison. Defaults to
            ``'origin'``.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.

    """
    requireGitRepo(cwd=cwd)

    branch = getCurrentBranch(cwd=cwd) or '(detached HEAD)'
    comparison = (
        getBranchComparison(branch, remote=remote, cwd=cwd) if branch != '(detached HEAD)' else ''
    )
    latest_tag = getLatestSemverTag(cwd=cwd)
    tag_str = latest_tag.toTag() if latest_tag else '(none)'
    last_commit = getLastCommitSummary(cwd=cwd) or '(no commits)'

    branch_line = branch
    if comparison:
        branch_line = f'{branch} [{comparison}]'

    col = 16
    print(f'{"Branch:":<{col}}{branch_line}')
    print(f'{"Last tag:":<{col}}{tag_str}')
    print(f'{"Last commit:":<{col}}{last_commit}')
