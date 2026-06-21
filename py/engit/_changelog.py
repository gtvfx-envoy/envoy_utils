"""engit changelog command — generate a changelog from GitHub releases.

Fetches all GitHub releases (via ``gh``), sorts by semantic version, and
renders the release titles and notes to stdout.
"""

from __future__ import annotations

import json
import subprocess
import shutil
from pathlib import Path

from ._exceptions import GhCliNotFoundError, GitHubError
from ._git import requireGitRepo


def _requireGh() -> None:
    if shutil.which('gh') is None:
        raise GhCliNotFoundError(
            "'gh' CLI not found on PATH. "
            "Install it from https://cli.github.com/ to use this command."
        )


def _gh(*args: str) -> str:
    _requireGh()
    result = subprocess.run(['gh', *args], capture_output=True, text=True)
    if result.returncode != 0:
        raise GitHubError(result.stderr.strip() or f"gh {' '.join(args)} failed.")
    return result.stdout.strip()


def runChangelog(
    *,
    tag: str | None = None,
    cwd: Path | None = None,
) -> None:
    """Print a changelog generated from GitHub releases.

    Fetches published releases from GitHub, filters to those whose tag names
    are valid semantic versions, sorts them newest-first, and prints each
    release title followed by its body.

    Args:
        tag: If given, show only that specific release instead of all releases.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the GitHub API call fails.

    """
    requireGitRepo(cwd=cwd)

    from ._semver import SemVer, SemVerError

    if tag:
        # Single-release view
        raw = _gh('release', 'view', tag, '--json', 'tagName,name,body')
        releases = json.loads(raw)
        if not isinstance(releases, list):
            releases = [releases]
    else:
        raw = _gh(
            'release', 'list',
            '--limit', '100',
            '--json', 'tagName,name,isPrerelease',
        )
        release_list = json.loads(raw) if raw else []

        # Fetch body for each release
        releases = []
        for item in release_list:
            try:
                detail = _gh(
                    'release', 'view', item['tagName'],
                    '--json', 'tagName,name,body',
                )
                releases.append(json.loads(detail))
            except GitHubError:
                continue

    # Filter to valid semver tags and sort newest-first.
    # Releases rank above prereleases of the same version.
    def _sortKey(r: dict) -> tuple:
        try:
            v = SemVer.parse(r.get('tagName', ''))
            # (major, minor, patch, is_release, prerelease_str)
            # is_release=1 for stable, 0 for prerelease → stable sorts higher
            is_release = 0 if v.prerelease else 1
            return (v.major, v.minor, v.patch, is_release, v.prerelease or '')
        except SemVerError:
            return (-1, -1, -1, 0, '')

    if not tag:
        releases = [r for r in releases if _sortKey(r) != (-1, -1, -1, 0, '')]
        releases.sort(key=_sortKey, reverse=True)

    if not releases:
        print('No releases found.')
        return

    if not tag:
        print('# Release notes\n')

    for release in releases:
        title = release.get('name') or release.get('tagName', '')
        body = (release.get('body') or '').strip()
        print(f'## {title}')
        if body:
            print()
            print(body)
        print()
