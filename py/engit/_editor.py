"""Shared editor helper for engit interactive review flows.

Opens content in the user's ``$EDITOR`` for review. Uses a git-style
comment convention: lines beginning with ``#`` are stripped before the
result is returned. Cancel is detected by checking whether the editor
wrote to the file (mtime change): closing without saving cancels, while
saving — even without edits — confirms.

Editor resolution order mirrors ``bfdeditor``:
    1. ``GIT_EDITOR``  — set by git and VS Code's ``code --wait`` workflow
    2. ``VISUAL``      — preferred full-screen editor (POSIX convention)
    3. ``EDITOR``      — fallback editor
    4. Platform default (``notepad`` on Windows, ``vim`` elsewhere)
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def _find_editor() -> list[str]:
    """Return the editor command as a list of tokens.

    Searches ``GIT_EDITOR``, ``VISUAL``, and ``EDITOR`` in that order,
    matching the priority used by bfdeditor/git.  Falls back to a
    platform default (``notepad`` on Windows, ``vim`` elsewhere).

    """
    posix = sys.platform != 'win32'
    for var in ('GIT_EDITOR', 'VISUAL', 'EDITOR'):
        value = os.environ.get(var, '').strip()
        if value:
            parts = shlex.split(value, posix=posix)
            # posix=False leaves surrounding quotes intact; strip them.
            if not posix:
                parts = [p.strip('"\"') for p in parts]
            return parts
    # Platform default — Classic Notepad on Windows (full path so it works even
    # when System32 is not on PATH), vim elsewhere.
    # To use a different editor set GIT_EDITOR / VISUAL / EDITOR in your shell
    # or in engit.bat before calling this tool.
    if sys.platform == 'win32':
        return [r'C:\Windows\notepad.exe']
    return ['vim']


def open_in_editor(content: str, filename: str = 'engit_edit.txt') -> str | None:
    """Present *content* in the user's editor and return the edited result.

    Editor resolution (mirrors bfdeditor / git priority):
        ``GIT_EDITOR`` → ``VISUAL`` → ``EDITOR`` → platform default.

    The editor command string is split with :func:`shlex.split` so that
    values like ``code --wait`` work correctly.

    Cancel detection uses the file's modification time: if the editor exits
    without the file being saved (mtime unchanged), ``None`` is returned.
    Saving the file — even without content changes — is treated as
    confirmation.

    Args:
        content: Initial text to display for editing.
        filename: Base filename for the temp file (e.g. ``'tag_v1.2.3.txt'``).
            Shown in the editor's title bar.

    Returns:
        The raw edited text, or ``None`` if the user canceled by closing the
        editor without saving.

    """
    cmd = _find_editor()
    editor_name = ' '.join(cmd)

    tmp_dir = Path(tempfile.mkdtemp(prefix='engit-'))
    tmp_path = tmp_dir / filename
    tmp_path.write_text(content, encoding='utf-8')

    mtime_before = tmp_path.stat().st_mtime_ns

    print(f'Opening editor ({editor_name}) — save the file to confirm, close without saving to cancel.')

    mtime_after = mtime_before  # sentinel: unchanged if editor raises
    raw = ''
    try:
        subprocess.run(cmd + [str(tmp_path)], check=True)
        mtime_after = tmp_path.stat().st_mtime_ns
        raw = tmp_path.read_text(encoding='utf-8')
    finally:
        shutil.rmtree(tmp_dir, ignore_errors=True)

    # No save — editor didn't touch the file.
    if mtime_after == mtime_before:
        return None

    return raw
