"""GitHub operations via the ``gh`` CLI.

All public functions delegate to the ``gh`` executable. A clear
:class:`~._exceptions.GhCliNotFoundError` is raised if ``gh`` is not
on PATH rather than letting the subprocess error surface raw.
"""

from __future__ import annotations

import json
import shutil
import subprocess

from ._exceptions import GitHubError, GhCliNotFoundError


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _require_gh() -> None:
    """Raise :class:`~._exceptions.GhCliNotFoundError` if ``gh`` is not found."""
    if shutil.which('gh') is None:
        raise GhCliNotFoundError(
            "'gh' CLI not found on PATH. "
            "Install it from https://cli.github.com/ to use this command."
        )


def _run(*args: str) -> str:
    """Run a ``gh`` command and return stripped stdout.

    Args:
        *args: Arguments passed to ``gh`` (excluding the ``gh`` prefix).

    Returns:
        Stripped stdout string.

    Raises:
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the command exits with a non-zero code.

    """
    _require_gh()
    cmd = ['gh', *args]
    result = subprocess.run(cmd, capture_output=True, text=True)

    if result.returncode != 0:
        raise GitHubError(result.stderr.strip() or f"gh {' '.join(args)} failed.")

    return result.stdout.strip()


# ---------------------------------------------------------------------------
# Release operations
# ---------------------------------------------------------------------------

def create_release(
    tag: str,
    title: str,
    notes: str,
    *,
    draft: bool = False,
    latest: bool = True,
) -> str:
    """Create a GitHub release for an existing tag.

    Args:
        tag: The git tag to release (e.g. ``'v1.2.3'``).
        title: Release title displayed on GitHub.
        notes: Release notes body (Markdown supported).
        draft: When ``True``, create the release as a draft.
        latest: When ``True``, mark this as the latest release.

    Returns:
        The URL of the created release.

    Raises:
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If release creation fails.

    """
    cmd: list[str] = [
        'release', 'create', tag,
        '--title', title,
        '--notes', notes,
    ]
    if draft:
        cmd.append('--draft')
    if latest:
        cmd.append('--latest')

    return _run(*cmd)


# ---------------------------------------------------------------------------
# Search operations
# ---------------------------------------------------------------------------

def search_repos(
    query: str,
    *,
    orgs: list[str] | None = None,
    limit: int = 20,
) -> list[dict]:
    """Search GitHub repositories matching *query*.

    When *orgs* is provided each organisation is searched sequentially and
    results are combined. Without orgs the search is global.

    Args:
        query: Search query string.
        orgs: List of GitHub organisation names to scope the search.
        limit: Maximum number of results per org (or global). Defaults to 20.

    Returns:
        A list of dicts with keys ``name``, ``full_name``, ``description``,
        ``url``, ``stars``, and ``updated_at``.

    Raises:
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the search fails.

    """
    _require_gh()

    targets: list[str | None] = orgs if orgs else [None]
    results: list[dict] = []
    seen: set[str] = set()

    for org in targets:
        scoped_query = f'org:{org} {query}' if org else query
        raw = _run(
            'search', 'repos',
            scoped_query,
            '--limit', str(limit),
            '--json', 'name,fullName,description,url,stargazersCount,updatedAt',
        )
        if not raw:
            continue
        try:
            items = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise GitHubError(f"Unexpected response from gh search: {exc}") from exc

        for item in items:
            full_name = item.get('fullName', '')
            if full_name in seen:
                continue
            seen.add(full_name)
            results.append({
                'name': item.get('name', ''),
                'full_name': full_name,
                'description': item.get('description') or '',
                'url': item.get('url', ''),
                'stars': item.get('stargazersCount', 0),
                'updated_at': item.get('updatedAt', ''),
            })

    return results
