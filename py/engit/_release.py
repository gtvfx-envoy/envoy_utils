"""engit release command — GitHub release creation from a tag.

Uses the selected tag's annotation as the release notes source-of-truth
(curated during ``engit tag``) and delegates to :mod:`._github` to create
the release via ``gh``."""

from __future__ import annotations

from pathlib import Path

from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    get_tag_annotation,
    get_sorted_semver_tags,
    get_commits_since,
    push_tag,
)
from ._github import create_release
from ._semver import SemVer


# ---------------------------------------------------------------------------
# Changelog helpers
# ---------------------------------------------------------------------------

def _build_draft_notes(tag: str, commits: list[str], *, initial: bool = False) -> str:
    """Build a Markdown draft changelog from a list of commit subjects.

    Args:
        tag: The release tag string (e.g. ``'v1.2.3'``).
        commits: One-line commit subject strings, most recent first.
        initial: When ``True``, uses the initial release default message
            instead of the generic no-changes fallback.

    Returns:
        A Markdown string suitable for a GitHub release body.

    """
    lines = [
        f'## {tag}',
        '',
        '### Changes',
        '',
    ]
    if commits:
        for msg in commits:
            lines.append(f'- {msg}')
    elif initial:
        lines.append('- This is the initial release.')
    else:
        lines.append('- No changes recorded since last tag.')
    lines.append('')
    return '\n'.join(lines)


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------

def run_release(
    *,
    tag: str | None = None,
    title: str | None = None,
    draft: bool = False,
    remote: str = 'origin',
    dry_run: bool = False,
    cwd: Path | None = None,
) -> None:
    """Push the local tag and create a GitHub release.

    Workflow:

    1. Resolve the target tag (latest local semver or explicit *tag*).
    2. Read the curated notes from the tag annotation (set by ``engit tag``).
    3. Push the local tag to *remote*.
    4. Create the release via ``gh release create``.

    Args:
        tag: Tag to release (e.g. ``'v1.2.3'``). Defaults to the most recent
            local semver tag — typically the one just created by ``engit tag``.
        title: Release title. Defaults to the tag string.
        draft: Create the release as a draft (not yet published).
        remote: Remote name to push the tag to. Defaults to ``'origin'``.
        dry_run: Print the planned release without pushing or creating it.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.NoTagsFoundError: If no local semver tags exist and
            *tag* is not supplied.
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the GitHub API call fails.
        ~._exceptions.GitError: If pushing the tag fails.

    """
    require_git_repo(cwd=cwd)

    # ---- Resolve tag ----
    if tag is None:
        latest = get_latest_semver_tag(cwd=cwd)
        if latest is None:
            from ._exceptions import NoTagsFoundError
            raise NoTagsFoundError(
                "No semantic version tags found locally. "
                "Run 'engit tag' first, or supply --tag explicitly."
            )
        tag = latest.to_tag()

    release_title = title or tag

    # ---- Resolve notes from curated tag annotation ----
    draft_notes = get_tag_annotation(tag, cwd=cwd)

    # Fallback for legacy/lightweight tags with no annotation.
    if not draft_notes:
        semver_tags = get_sorted_semver_tags(cwd=cwd)
        try:
            tag_index = semver_tags.index(tag)
            prev_tag = semver_tags[tag_index + 1] if tag_index + 1 < len(semver_tags) else None
        except ValueError:
            prev_tag = None

        commits = get_commits_since(prev_tag, cwd=cwd) if prev_tag else []
        draft_notes = _build_draft_notes(tag, commits, initial=not prev_tag)

    notes = draft_notes

    # ---- Dry run ----
    if dry_run:
        print('\n[dry-run] Would create GitHub release:')
        print(f'  Tag:    {tag}')
        print(f'  Title:  {release_title}')
        print(f'  Remote: {remote}')
        print(f'  Draft:  {draft}')
        print()
        print('--- Notes ---')
        print(notes)
        print('--- End ---')
        return

    # ---- Push the local tag ----
    push_tag(tag, remote=remote, cwd=cwd)
    print(f'Pushed {tag} to {remote}')

    # ---- Create the GitHub release ----
    url = create_release(tag, release_title, notes, draft=draft)
    print(f'Release created: {url}')
