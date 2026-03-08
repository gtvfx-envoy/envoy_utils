"""engit release command — GitHub release creation from a tag.

Aggregates commit messages since the last tag into a draft changelog, opens
the user's ``$EDITOR`` (or a simple in-process prompt) for review and editing,
then delegates to :mod:`._github` to create the release via ``gh``.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path

from ._exceptions import GitHubError
from ._git import (
    require_git_repo,
    get_latest_semver_tag,
    get_commits_since,
    get_all_commits,
    get_remote_url,
    push_tag,
)
from ._github import create_release
from ._semver import SemVer


# ---------------------------------------------------------------------------
# Changelog helpers
# ---------------------------------------------------------------------------

def _build_draft_notes(tag: str, commits: list[str]) -> str:
    """Build a Markdown draft changelog from a list of commit subjects.

    Args:
        tag: The release tag string (e.g. ``'v1.2.3'``).
        commits: One-line commit subject strings, most recent first.

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
    else:
        lines.append('- No changes recorded since last tag.')
    lines.append('')
    return '\n'.join(lines)


def _open_in_editor(content: str) -> str:
    """Open *content* in the user's ``$EDITOR`` for review and return the result.

    Falls back to a simple terminal prompt when no editor is configured.

    Args:
        content: Initial text to present for editing.

    Returns:
        The (possibly modified) text after the editor closes.

    """
    editor = os.environ.get('EDITOR') or os.environ.get('VISUAL')

    if editor:
        with tempfile.NamedTemporaryFile(
            mode='w',
            suffix='.md',
            delete=False,
            encoding='utf-8',
        ) as tmp:
            tmp.write(content)
            tmp_path = tmp.name

        try:
            subprocess.run([editor, tmp_path], check=True)
            return Path(tmp_path).read_text(encoding='utf-8')
        finally:
            Path(tmp_path).unlink(missing_ok=True)

    # No editor — print the draft and ask for confirmation/inline edit.
    print('\n--- Draft release notes (no $EDITOR set) ---')
    print(content)
    print('--- End of draft ---')
    print()
    print('Press Enter to use as-is, or type a replacement (Ctrl+C to abort):')
    user_input = input().strip()
    return user_input if user_input else content


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------

def run_release(
    *,
    tag: str | None = None,
    title: str | None = None,
    draft: bool = False,
    push: bool = False,
    remote: str = 'origin',
    yes: bool = False,
    dry_run: bool = False,
    cwd: Path | None = None,
) -> None:
    """Create a GitHub release, optionally after pushing the tag.

    Workflow:

    1. Determine the target tag (latest semver or explicit *tag*).
    2. Gather commit messages since the previous tag.
    3. Present a Markdown draft to the user for review/editing.
    4. Create the release via ``gh release create``.

    Args:
        tag: The tag to release. Defaults to the most recent semver tag.
        title: Release title. Defaults to the tag string.
        draft: Create the release as a draft (not yet published).
        push: Push the tag to *remote* before creating the release.
        remote: Remote name. Defaults to ``'origin'``.
        yes: Skip the editor and use the auto-generated notes unchanged.
        dry_run: Print the planned release without creating it.
        cwd: Git working directory. Defaults to the current directory.

    Raises:
        ~._exceptions.NotAGitRepoError: If not inside a git repo.
        ~._exceptions.NoTagsFoundError: If no semver tags exist and *tag*
            is not supplied.
        ~._exceptions.GhCliNotFoundError: If ``gh`` is not installed.
        ~._exceptions.GitHubError: If the GitHub API call fails.

    """
    require_git_repo(cwd=cwd)

    # ---- Resolve tag ----
    if tag is None:
        latest = get_latest_semver_tag(cwd=cwd)
        if latest is None:
            from ._exceptions import NoTagsFoundError
            raise NoTagsFoundError(
                "No semantic version tags found. "
                "Run 'engit tag' first, or supply --tag explicitly."
            )
        tag = latest.to_tag()

    release_title = title or tag

    # ---- Optional push ----
    if push:
        if dry_run:
            print(f'[dry-run] Would push tag {tag} to {remote}')
        else:
            push_tag(tag, remote=remote, cwd=cwd)
            print(f'Pushed {tag} to {remote}')

    # ---- Build commit list since the *previous* tag ----
    # Walk through semver tags sorted newest-first; the second one is the
    # predecessor of *tag*.
    import subprocess as _sp
    raw_tags = _sp.run(
        ['git', 'tag', '--list', '--sort=-version:refname'],
        capture_output=True,
        text=True,
        cwd=cwd,
    ).stdout.strip().splitlines()

    from ._semver import SemVer
    from ._exceptions import SemVerError

    semver_tags = []
    for t in raw_tags:
        try:
            SemVer.parse(t.strip())
            semver_tags.append(t.strip())
        except SemVerError:
            continue

    try:
        tag_index = semver_tags.index(tag)
        prev_tag = semver_tags[tag_index + 1] if tag_index + 1 < len(semver_tags) else None
    except ValueError:
        prev_tag = None

    if prev_tag:
        commits = get_commits_since(prev_tag, cwd=cwd)
    else:
        commits = get_all_commits(cwd=cwd)

    # ---- Draft notes ----
    draft_notes = _build_draft_notes(tag, commits)

    if yes or dry_run:
        notes = draft_notes
    else:
        notes = _open_in_editor(draft_notes)

    # ---- Create release ----
    if dry_run:
        print(f'\n[dry-run] Would create GitHub release:')
        print(f'  Tag:   {tag}')
        print(f'  Title: {release_title}')
        print(f'  Draft: {draft}')
        print()
        print('--- Notes ---')
        print(notes)
        print('--- End ---')
        return

    url = create_release(tag, release_title, notes, draft=draft)
    print(f'Release created: {url}')
