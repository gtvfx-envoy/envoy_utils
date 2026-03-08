"""engit tag command — semantic version tag creation.

Handles bumping the current version (major/minor/patch) or accepting an
explicit version, enforces SemVer, then presents the proposed tag and commit
log for confirmation in the user's editor before creating a local annotated
tag. Push and release are handled separately by ``engit release``.
"""

from __future__ import annotations

from pathlib import Path

from engit._editor import open_in_editor
from ._exceptions import NoTagsFoundError
from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    get_sorted_semver_tags,
    get_commits_since,
    create_tag,
    tag_exists,
)
from ._semver import SemVer


def resolve_next_version(
    *,
    bump: str | None = None,
    version: str | None = None,
    cwd: Path | None = None,
) -> SemVer:
    """Determine the next version to tag.

    Exactly one of *bump* or *version* must be supplied.

    Args:
        bump: One of ``'major'``, ``'minor'``, or ``'patch'``.
            Increments the corresponding component of the current latest tag.
        version: An explicit version string such as ``'1.2.3'`` or ``'v1.2.3'``.
            Validated against the SemVer pattern before use.
        cwd: Git working directory. Defaults to the current directory.

    Returns:
        The resolved :class:`~._semver.SemVer` to use for the new tag.

    Raises:
        ValueError: If neither or both of *bump* / *version* are supplied,
            or if *bump* is not a recognised component.
        ~._exceptions.SemVerError: If *version* is not valid SemVer.
        ~._exceptions.NoTagsFoundError: If *bump* is requested but no prior
            semver tag exists to increment from.

    """
    if bool(bump) == bool(version):
        raise ValueError("Provide exactly one of 'bump' or 'version'.")

    if version is not None:
        return SemVer.parse(version)

    # bump mode
    current = get_latest_semver_tag(cwd=cwd)
    if current is None:
        raise NoTagsFoundError(
            "No semantic version tags found in this repository. "
            "Use --version to supply an explicit first version (e.g. --version 0.0.1)."
        )

    if bump is None:
        raise ValueError("Expected a bump component but received None. This is a bug.")
    
    bump = bump.lower()
    if bump == 'major':
        return current.bump_major()
    if bump == 'minor':
        return current.bump_minor()
    if bump == 'patch':
        return current.bump_patch()

    raise ValueError(f"Unknown bump component '{bump}'. Use 'major', 'minor', or 'patch'.")


def _strip_comments(text: str) -> str:
    """Strip ``#``-prefixed comment lines and trim blank edges.

    Mirrors the comment-stripping that ``git tag -a`` performs when the
    editor is invoked interactively.  Any line whose first non-space
    character is ``#`` is removed; the remainder is stripped of leading
    and trailing blank lines.

    Args:
        text: Raw editor buffer contents.

    Returns:
        Cleaned annotation text, or an empty string if nothing remains.

    """
    kept = [
        line for line in text.splitlines()
        if not line.lstrip().startswith('#')
    ]
    # Drop trailing blank lines
    while kept and not kept[-1].strip():
        kept.pop()
    # Drop leading blank lines
    while kept and not kept[0].strip():
        kept.pop(0)
    return '\n'.join(kept)


def _build_tag_draft(
    tag: str,
    default_annotation: str,
    commits: list[str],
    prev_tag: str | None,
) -> str:
    """Build the editor draft for tag confirmation.

    Mirrors blgit's ``tag-template``: editable content comes first, with a
    compact ``#``-comment block at the bottom — the same convention as
    ``git commit``.  Lines starting with ``#`` are stripped before the
    annotation is stored.

    The stored annotation will be::

        Release v1.2.3

        - Commit summary one
        - Commit summary two

    The first line becomes the GitHub release title; subsequent lines
    become the release body.

    Args:
        tag: The proposed tag string (e.g. ``'v1.2.3'``).
        default_annotation: Default first line (release title).
        commits: Commit subject lines since the previous tag.
        prev_tag: The previous semver tag string, or ``None`` if first tag.

    Returns:
        Draft string ready to open in an editor.

    """
    if commits:
        body_lines = [f'- {msg}' for msg in commits]
    elif prev_tag is None:
        body_lines = ['This is the initial release.']
    else:
        body_lines = ['This is a no-change release.']

    # Content first — mirrors blgit's tag-template structure so the cursor
    # lands immediately on the editable text, not a wall of instructions.
    lines: list[str] = [
        default_annotation,
        '',
    ] + body_lines + [
        '',
        '#',
        f'# Write a message for tag: {tag}',
    ]

    if prev_tag:
        lines.append(f'# Previous tag: {prev_tag}')
        lines.append(f'# Commits above pre-populated from git log since {prev_tag}.')
    else:
        lines.append('# First tag in this repository.')

    lines += [
        "# Lines starting with '#' will be ignored.",
        '# Save the file to confirm. Close without saving to cancel.',
    ]

    return '\n'.join(lines) + '\n'


def run_tag(
    *,
    bump: str | None = None,
    version: str | None = None,
    message: str | None = None,
    print_only: bool = False,
    dry_run: bool = False,
    cwd: Path | None = None,
) -> SemVer | None:
    """Create a local annotated git tag for the next semantic version.

    Opens the user's editor to review and curate release notes before
    creating the tag. The resulting non-comment text is stored as the
    annotated tag message. Closing with no non-comment content cancels.

    Args:
        bump: Version component to increment — ``'major'``, ``'minor'``,
            or ``'patch'``.
        version: Explicit full version string (overrides *bump*).
        message: Override the default release title line. Skips the editor.
        print_only: Print the computed next version and exit without tagging.
        dry_run: When ``True``, print the planned tag without creating it.
        cwd: Git working directory. Defaults to the current directory.

    Returns:
        The :class:`~._semver.SemVer` that was tagged, or ``None`` if the
        user canceled in the editor.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.SemVerError: If the supplied *version* is invalid.
        ~._exceptions.NoTagsFoundError: If bump is used with no existing tags.
        ~._exceptions.GitError: If tag creation fails.

    """
    require_git_repo(cwd=cwd)

    next_ver = resolve_next_version(bump=bump, version=version, cwd=cwd)
    tag_name = next_ver.to_tag()
    default_annotation = message or f'Release {tag_name}'

    if print_only:
        print(tag_name)
        return next_ver

    if dry_run:
        print(f'[dry-run] Would create tag: {tag_name}')
        return next_ver

    # ---- Guard: refuse to overwrite an existing tag ----
    if tag_exists(tag_name, cwd=cwd):
        from ._exceptions import GitError
        raise GitError(
            f"Tag '{tag_name}' already exists. "
            "Use --version to supply a different version."
        )

    # ---- Build commit context for the editor draft ----
    semver_tags = get_sorted_semver_tags(cwd=cwd)
    # The proposed tag may not exist yet; find the current latest as prev.
    prev_tag = semver_tags[0] if semver_tags else None

    if prev_tag:
        commits = get_commits_since(prev_tag, cwd=cwd)
    else:
        commits = []  # First tag — use default initial release message.

    # ---- If a message was supplied explicitly, skip the editor ----
    if message is not None:
        annotation = _strip_comments(message)
    else:
        draft = _build_tag_draft(tag_name, default_annotation, commits, prev_tag)
        raw = open_in_editor(draft, filename='TAG_EDITMSG')
        if raw is None:
            print('Tag aborted.')
            return None
        annotation = _strip_comments(raw)

    if not annotation:
        print('Tag aborted: empty annotation after removing comments.')
        return None

    create_tag(tag_name, annotation, cwd=cwd)
    print(f'Created tag: {tag_name}')
    print(f'Run \'engit release\' when ready to publish.')
    return next_ver
