"""engit release command — GitHub release creation from a tag.

Uses the selected tag's annotation as the release notes source-of-truth
(curated during ``engit tag``) and delegates to :mod:`._github` to create
the release via ``gh``."""

from __future__ import annotations

from pathlib import Path

from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    get_current_branch,
    get_tag_annotation,
    get_sorted_semver_tags,
    get_commits_since,
    push_branch_and_tag,
)
from ._github import create_release, release_exists, get_release_url


# ---------------------------------------------------------------------------
# Changelog helpers
# ---------------------------------------------------------------------------

def _build_draft_notes(tag: str, commits: list[str], *, initial: bool = False) -> str:
    """Build a plain-text release body from a list of commit subjects.

    Mirrors ``blgit``'s ``generateReleaseNotes``: a bulleted commit list,
    or a plain sentence when there are no meaningful changes.

    Args:
        tag: The release tag string (e.g. ``'v1.2.3'``).
        commits: One-line commit subject strings, most recent first.
        initial: When ``True``, uses the initial release message instead
            of the generic no-changes fallback.

    Returns:
        A plain-text string suitable for a GitHub release body.

    """
    if commits:
        return '\n'.join(f'- {msg}' for msg in commits)
    if initial:
        return 'This is the initial release.'
    return 'This is a no-change release.'


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------

def _parse_annotation(annotation: str) -> tuple[str, str]:
    """Split a tag annotation into a release title and body.

    Mirrors ``blgit``'s ``releaseCommand`` parsing:

    * Strip ``#``-prefixed comment lines (defensive, in case an old tag has
      them from a pre-fix engit version).
    * First non-empty line → release title.
    * Everything after the first blank separator → release body.

    Returns:
        ``(title, body)`` tuple.

    """
    # Defensive: strip comment lines from legacy annotations
    clean_lines = [
        line for line in annotation.splitlines()
        if not line.lstrip().startswith('#')
    ]
    # Drop leading blank lines
    while clean_lines and not clean_lines[0].strip():
        clean_lines.pop(0)

    if not clean_lines:
        return ('', '')

    title = clean_lines[0].strip()
    body_lines = clean_lines[1:]
    # Strip leading blank separator between title and body
    while body_lines and not body_lines[0].strip():
        body_lines.pop(0)
    return title, '\n'.join(body_lines).strip()


def run_release(
    *,
    tag: str | None = None,
    title: str | None = None,
    draft: bool = False,
    remote: str = 'origin',
    print_only: bool = False,
    dry_run: bool = False,
    cwd: Path | None = None,
) -> None:
    """Push the local tag and create a GitHub release.

    Workflow:

    1. Resolve the target tag (latest local semver or explicit *tag*).
    2. Read the curated notes from the tag annotation (set by ``engit tag``).
    3. Push the current branch and tag to *remote*.
    4. Create the release via ``gh release create``.

    Args:
        tag: Tag to release (e.g. ``'v1.2.3'``). Defaults to the most recent
            local semver tag — typically the one just created by ``engit tag``.
        title: Release title override. Defaults to the first line of the tag
            annotation.
        draft: Create the release as a draft (not yet published).
        remote: Remote name to push the tag to. Defaults to ``'origin'``.
        print_only: Print the resolved release notes and exit without publishing.
        dry_run: Print a full plan without pushing or creating the release.
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

    # ---- Resolve release notes ----
    annotation = get_tag_annotation(tag, cwd=cwd)

    # Fallback: legacy / lightweight tags with no annotation.
    # Mirrors blgit: synthesise title + body in the same format that
    # ``engit tag`` now stores (first line = title, blank, body bullets).
    if not annotation:
        semver_tags = get_sorted_semver_tags(cwd=cwd)
        try:
            tag_index = semver_tags.index(tag)
            prev_tag = semver_tags[tag_index + 1] if tag_index + 1 < len(semver_tags) else None
        except ValueError:
            prev_tag = None
        commits = get_commits_since(prev_tag, cwd=cwd) if prev_tag else []
        body = _build_draft_notes(tag, commits, initial=not prev_tag)
        annotation = f'Release {tag}\n\n{body}'

    # ---- Parse title and body from annotation ----
    parsed_title, notes = _parse_annotation(annotation)
    release_title = title or parsed_title or tag

    # ---- Detect prerelease from semver tag ----
    # engit's SemVer only supports plain MAJOR.MINOR.PATCH; prerelease
    # metadata (e.g. -alpha.1) is not currently produced by engit tag.
    # Reserved for future use when prerelease suffixes are supported.
    is_prerelease = False

    # ---- Print-only mode ----
    if print_only:
        print(f'Tag:   {tag}')
        print(f'Title: {release_title}')
        print()
        print(notes)
        return

    # ---- Dry run ----
    if dry_run:
        print('\n[dry-run] Would create GitHub release:')
        print(f'  Tag:        {tag}')
        print(f'  Title:      {release_title}')
        print(f'  Remote:     {remote}')
        print(f'  Draft:      {draft}')
        print(f'  Prerelease: {is_prerelease}')
        print()
        print('--- Notes ---')
        print(notes)
        print('--- End ---')
        return

    # ---- Check for existing release ----
    if release_exists(tag):
        existing_url = get_release_url(tag)
        print(f'A release for {tag} already exists: {existing_url}')
        return

    # ---- Push branch + tag ----
    branch = get_current_branch(cwd=cwd)
    if branch:
        push_branch_and_tag(tag, branch, remote=remote, cwd=cwd)
        print(f'Pushed {branch} and {tag} to {remote}')
    else:
        from ._git import push_tag
        push_tag(tag, remote=remote, cwd=cwd)
        print(f'Pushed {tag} to {remote} (detached HEAD — branch not pushed)')

    # ---- Create the GitHub release ----
    url = create_release(tag, release_title, notes, draft=draft, prerelease=is_prerelease)
    print(f'Release created: {url}')
