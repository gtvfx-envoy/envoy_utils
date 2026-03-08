"""engit tag command — semantic version tag creation.

Handles bumping the current version (major/minor/patch) or accepting an
explicit version, enforces SemVer, then presents the proposed tag and commit
log for confirmation in the user's editor before creating a local annotated
tag. Push and release are handled separately by ``engit release``.
"""

from __future__ import annotations

from pathlib import Path

from ._editor import open_in_editor
from ._exceptions import NoTagsFoundError
from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    get_sorted_semver_tags,
    get_commits_since,
    get_all_commits,
    create_tag,
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


def _build_tag_draft(
    tag: str,
    default_annotation: str,
    commits: list[str],
    prev_tag: str | None,
) -> str:
    """Build the editor draft for tag confirmation.

    The non-comment lines form the default tag annotation. Comment lines
    (``#``-prefixed) provide context — the proposed tag name, previous tag,
    and the commit list — and are stripped before the annotation is used.

    Args:
        tag: The proposed tag string (e.g. ``'v1.2.3'``).
        default_annotation: Default first line of the annotation.
        commits: Commit subject lines since the previous tag.
        prev_tag: The previous semver tag string, or ``None`` if first tag.

    Returns:
        Markdown-ish draft string ready to open in an editor.

    """
    lines: list[str] = [
        default_annotation,
        '',
        f'# Proposed tag : {tag}',
    ]

    if prev_tag:
        lines.append(f'# Previous tag : {prev_tag}')
        lines.append(f'# Commits since {prev_tag}:')
    else:
        lines.append('# First tag in this repository.')
        lines.append('# All commits:')

    if commits:
        for msg in commits:
            lines.append(f'#   {msg}')
    else:
        lines.append('#   (no commits recorded)')

    lines += [
        '#',
        "# Lines beginning with '#' are ignored.",
        '# An empty message after removing comments will abort the tag.',
    ]

    return '\n'.join(lines) + '\n'


def run_tag(
    *,
    bump: str | None = None,
    version: str | None = None,
    message: str | None = None,
    dry_run: bool = False,
    cwd: Path | None = None,
) -> SemVer | None:
    """Create a local annotated git tag for the next semantic version.

    Opens the user's editor to review the proposed tag name and commit list
    before committing. The tag annotation is editable in the editor. Closing
    with no non-comment content cancels the operation.

    Args:
        bump: Version component to increment — ``'major'``, ``'minor'``,
            or ``'patch'``.
        version: Explicit full version string (overrides *bump*).
        message: Override the default tag annotation. Skips the editor.
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

    if dry_run:
        print(f'[dry-run] Would create tag: {tag_name}')
        return next_ver

    # ---- Build commit context for the editor draft ----
    semver_tags = get_sorted_semver_tags(cwd=cwd)
    # The proposed tag may not exist yet; find the current latest as prev.
    prev_tag = semver_tags[0] if semver_tags else None

    if prev_tag:
        commits = get_commits_since(prev_tag, cwd=cwd)
    else:
        commits = get_all_commits(cwd=cwd)

    # ---- If a message was supplied explicitly, skip the editor ----
    if message is not None:
        annotation = message
    else:
        draft = _build_tag_draft(tag_name, default_annotation, commits, prev_tag)
        annotation = open_in_editor(draft)
        if annotation is None:
            print('Tag aborted.')
            return None

    create_tag(tag_name, annotation, cwd=cwd)
    print(f'Created tag: {tag_name}')
    print(f'Run \'engit release\' when ready to publish.')
    return next_ver
