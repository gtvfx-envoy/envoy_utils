"""engit web command — open the current repository on GitHub.

Resolves the remote URL and opens it in the default web browser.
"""

from __future__ import annotations

import webbrowser
from pathlib import Path

from ._exceptions import GitError
from ._git import getCurrentBranch, getRemoteUrl, requireGitRepo


def _toHttpsUrl(remote_url: str) -> str:
    """Normalise an SSH or HTTPS remote URL to an HTTPS browser URL.

    Handles:
    * ``git@github.com:owner/repo.git`` → ``https://github.com/owner/repo``
    * ``https://github.com/owner/repo.git`` → ``https://github.com/owner/repo``
    * ``https://github.com/owner/repo`` → unchanged

    """
    url = remote_url.strip()

    # SSH format: git@host:owner/repo.git
    if url.startswith('git@') and ':' in url:
        host, path = url[len('git@') :].split(':', 1)
        url = f'https://{host}/{path}'

    # Strip trailing .git
    if url.endswith('.git'):
        url = url[:-4]

    return url


def runWeb(
    *,
    branch: str | None = None,
    remote: str = 'origin',
    cwd: Path | None = None,
) -> None:
    """Open the current repository on GitHub in a web browser.

    Args:
        branch: Branch or tag to view. Defaults to the currently checked-out
            branch. Pass ``None`` to open the repository root.
        remote: Remote whose URL is used. Defaults to ``'origin'``.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.GitError: If the remote URL cannot be determined.

    """
    requireGitRepo(cwd=cwd)

    raw_url = getRemoteUrl(remote=remote, cwd=cwd)
    if not raw_url:
        raise GitError(f"Remote '{remote}' has no URL. Check your git remote configuration.")

    base_url = _toHttpsUrl(raw_url)

    # Append the branch/tree path if we have one
    resolved_branch = branch or getCurrentBranch(cwd=cwd)
    if resolved_branch:
        url = f'{base_url}/tree/{resolved_branch}'
    else:
        url = base_url

    print(f'Opening {url}')
    try:
        webbrowser.open(url, new=2)
    except webbrowser.Error as exc:
        raise GitError(f'Could not open web browser: {exc}') from exc
