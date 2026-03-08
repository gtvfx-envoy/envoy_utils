"""engit tag command — semantic version tag creation.

Handles bumping the current version (major/minor/patch) or accepting an
explicit version, enforces SemVer, creates an annotated git tag, and
optionally pushes it to the remote.

"""

from __future__ import annotations

from pathlib import Path

from ._exceptions import SemVerError, NoTagsFoundError
from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    create_tag,
    push_tag,
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

    bump = bump.lower()
    if bump == 'major':
        return current.bump_major()
    if bump == 'minor':
        return current.bump_minor()
    if bump == 'patch':
        return current.bump_patch()

    raise ValueError(f"Unknown bump component '{bump}'. Use 'major', 'minor', or 'patch'.")


def run_tag(
    *,
    bump: str | None = None,
    version: str | None = None,
    message: str | None = None,
    push: bool = False,
    remote: str = 'origin',
    dry_run: bool = False,
    cwd: Path | None = None,
) -> SemVer:
    """Create an annotated git tag for the next semantic version.

    Args:
        bump: Version component to increment — ``'major'``, ``'minor'``,
            or ``'patch'``.
        version: Explicit full version string (overrides *bump*).
        message: Custom tag annotation. Defaults to ``'Release vMAJOR.MINOR.PATCH'``.
        push: When ``True``, push the tag to *remote* after creation.
        remote: Remote name to push to. Defaults to ``'origin'``.
        dry_run: When ``True``, print the planned tag without creating it.
        cwd: Git working directory. Defaults to the current directory.

    Returns:
        The :class:`~._semver.SemVer` that was (or would be) tagged.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.SemVerError: If the supplied *version* is invalid.
        ~._exceptions.NoTagsFoundError: If bump is used with no existing tags.
        ~._exceptions.GitError: If tag creation or push fails.

    """
    require_git_repo(cwd=cwd)

    next_ver = resolve_next_version(bump=bump, version=version, cwd=cwd)
    tag_name = next_ver.to_tag()
    annotation = message or f'Release {tag_name}'

    if dry_run:
        print(f'[dry-run] Would create tag: {tag_name}')
        print(f'[dry-run] Annotation: {annotation}')
        if push:
            print(f'[dry-run] Would push to: {remote}')
        return next_ver

    create_tag(tag_name, annotation, cwd=cwd)
    print(f'Created tag: {tag_name}')

    if push:
        push_tag(tag_name, remote=remote, cwd=cwd)
        print(f'Pushed {tag_name} to {remote}')

    return next_ver
