"""engit search command — GitHub repository search.

Reads default organisations from the ``ENVOY_GITHUB_ORGS`` environment
variable (semicolon-separated) and supports an explicit ``--org`` override.
"""

from __future__ import annotations

import os

from ._exceptions import GitHubError
from ._github import search_repos


#: Environment variable that holds the default semicolon-separated list of
#: GitHub organisation names to scope repository searches.
ORGS_ENV_VAR = 'ENVOY_GITHUB_ORGS'


def _parse_orgs(raw: str) -> list[str]:
    """Split a semicolon-separated org string into a clean list.

    Args:
        raw: Raw string such as ``'gtvfx-contrib;gtvfx-elvtr;gtvfx'``.

    Returns:
        A list of non-empty org name strings.

    """
    return [o.strip() for o in raw.replace(',', ';').split(';') if o.strip()]


def run_search(
    query: str,
    *,
    orgs: list[str] | None = None,
    limit: int = 20,
) -> None:
    """Search GitHub repositories and print formatted results.

    Organisation scope is resolved in priority order:

    1. Explicit *orgs* argument (``--org`` flags on the CLI).
    2. ``ENVOY_GITHUB_ORGS`` environment variable.
    3. Global search (no org scope) if neither is set.

    Args:
        query: Search query string.
        orgs: Organisation names to scope the search.
        limit: Maximum results per organisation. Defaults to 20.

    Raises:
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the search fails.

    """
    # Resolve org list
    effective_orgs: list[str] | None = orgs

    if not effective_orgs:
        env_orgs_raw = os.environ.get(ORGS_ENV_VAR, '')
        if env_orgs_raw:
            effective_orgs = _parse_orgs(env_orgs_raw)

    results = search_repos(query, orgs=[*effective_orgs] if effective_orgs else None, limit=limit)

    if not results:
        scope = ', '.join(o for o in effective_orgs if o) if effective_orgs else 'GitHub (global)'
        print(f"No repositories found matching '{query}' in: {scope}")
        return

    _print_results(results, query=query, orgs=effective_orgs)


def _print_results(results: list[dict], *, query: str, orgs: list[str] | None) -> None:
    """Render search results to stdout.

    Args:
        results: List of repo dicts from :func:`~._github.search_repos`.
        query: The original search query (for the header line).
        orgs: Org scope used (for the header line).

    """
    scope = ', '.join(orgs) if orgs else 'GitHub (global)'
    print(f"\nSearch: '{query}'  |  Scope: {scope}  |  {len(results)} result(s)\n")

    for repo in results:
        stars = repo['stars']
        name = repo['full_name']
        desc = repo['description']
        url = repo['url']
        updated = repo['updated_at'][:10] if repo['updated_at'] else ''

        print(f"  {name}")
        if desc:
            print(f"    {desc}")
        print(f"    {url}  ★ {stars}  updated: {updated}")
        print()
