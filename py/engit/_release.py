"""engit release command — GitHub release creation from a tag.

Uses the selected tag's annotation as the release notes source-of-truth
(curated during ``engit tag``) and delegates to :mod:`._github` to create
the release via ``gh``.
"""

from __future__ import annotations

from pathlib import Path

from ._git import (
    getCommitsSince,
    getCurrentBranch,
    getLatestSemverTag,
    getSortedSemverTags,
    getTagAnnotation,
    pushBranchAndTag,
    requireGitRepo,
)
from ._github import createRelease, getReleaseUrl, releaseExists

# ---------------------------------------------------------------------------
# Changelog helpers
# ---------------------------------------------------------------------------


def _buildDraftNotes(tag: str, commits: list[str], *, initial: bool = False) -> str:
    """Build a plain-text release body from a list of commit subjects.

    Builds a bulleted commit list, or a plain sentence when there are no
    meaningful changes.

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


def _parseAnnotation(annotation: str) -> tuple[str, str]:
    """Split a tag annotation into a release title and body.

    Parsing rules:

    * Strip ``#``-prefixed comment lines (defensive, in case an old tag has
      them from a pre-fix engit version).
    * First non-empty line → release title.
    * Everything after the first blank separator → release body.

    Returns:
        ``(title, body)`` tuple.

    """
    # Defensive: strip comment lines from legacy annotations
    clean_lines = [line for line in annotation.splitlines() if not line.lstrip().startswith('#')]
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


def runRelease(
    *,
    tag: str | None = None,
    title: str | None = None,
    draft: bool = False,
    remote: str = 'origin',
    print_only: bool = False,
    dry_run: bool = False,
    generate_notes: bool = False,
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
        generate_notes: When ``True``, append GitHub auto-generated "What's
            Changed" notes from merged PRs to the release body.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.NoTagsFoundError: If no local semver tags exist and
            *tag* is not supplied.
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the GitHub API call fails.
        ~._exceptions.GitError: If pushing the tag fails.

    """
    requireGitRepo(cwd=cwd)

    # ---- Resolve tag ----
    if tag is None:
        latest = getLatestSemverTag(cwd=cwd)
        if latest is None:
            from ._exceptions import NoTagsFoundError

            raise NoTagsFoundError(
                "No semantic version tags found locally. "
                "Run 'engit tag' first, or supply --tag explicitly."
            )
        tag = latest.toTag()

    # ---- Resolve release notes ----
    annotation = getTagAnnotation(tag, cwd=cwd)

    # Fallback: legacy / lightweight tags with no annotation.
    # Synthesise title + body in the same format engit tag stores
    # (first line = title, blank line, body bullets).
    if not annotation:
        semver_tags = getSortedSemverTags(cwd=cwd)
        try:
            tag_index = semver_tags.index(tag)
            prev_tag = semver_tags[tag_index + 1] if tag_index + 1 < len(semver_tags) else None
        except ValueError:
            prev_tag = None
        commits = getCommitsSince(prev_tag, cwd=cwd) if prev_tag else []
        body = _buildDraftNotes(tag, commits, initial=not prev_tag)
        annotation = f'Release {tag}\n\n{body}'

    # ---- Parse title and body from annotation ----
    parsed_title, notes = _parseAnnotation(annotation)
    release_title = title or parsed_title or tag

    # ---- Detect prerelease from tag ----
    from ._semver import SemVer, SemVerError

    try:
        _tag_ver = SemVer.parse(tag)
        is_prerelease = _tag_ver.prerelease is not None
    except SemVerError:
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
        print(f'  Tag:             {tag}')
        print(f'  Title:           {release_title}')
        print(f'  Remote:          {remote}')
        print(f'  Draft:           {draft}')
        print(f'  Prerelease:      {is_prerelease}')
        print(f'  Generate notes:  {generate_notes}')
        print()
        print('--- Notes ---')
        print(notes)
        print('--- End ---')
        return

    # ---- Check for existing release ----
    if releaseExists(tag):
        existing_url = getReleaseUrl(tag)
        print(f'A release for {tag} already exists: {existing_url}')
        return

    # ---- Push branch + tag ----
    branch = getCurrentBranch(cwd=cwd)
    if branch:
        pushBranchAndTag(tag, branch, remote=remote, cwd=cwd)
        print(f'Pushed {branch} and {tag} to {remote}')
    else:
        from ._git import pushTag

        pushTag(tag, remote=remote, cwd=cwd)
        print(f'Pushed {tag} to {remote} (detached HEAD — branch not pushed)')

    # ---- Create the GitHub release ----
    url = createRelease(
        tag,
        release_title,
        notes,
        draft=draft,
        prerelease=is_prerelease,
        generate_notes=generate_notes,
    )
    print(f'Release created: {url}')
