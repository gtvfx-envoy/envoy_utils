"""Shared editor helper for engit interactive review flows.

Opens content in the user's ``$EDITOR`` for review. Uses a git-style
comment convention: lines beginning with ``#`` are stripped before the
result is returned. Cancel is detected by checking whether the editor
wrote to the file (mtime change): closing without saving cancels, while
saving — even without edits — confirms.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path


def open_in_editor(content: str) -> str | None:
    """Present *content* in the user's ``$EDITOR`` and return the edited result.

    Lines beginning with ``#`` are treated as comments and stripped from the
    returned string, matching git's commit-message convention.

    Cancel detection uses the file's modification time: if the editor exits
    without the file being saved, the mtime is unchanged and ``None`` is
    returned. Saving the file — even without edits — is treated as confirmation
    and returns the content with comments stripped.

    Falls back to a simple terminal prompt when no ``$EDITOR`` is set.

    Args:
        content: Initial text to display for editing.

    Returns:
        The edited text with comment lines removed, or ``None`` if the user
        canceled by closing the editor without saving.

    """
    editor = os.environ.get('EDITOR') or os.environ.get('VISUAL')

    if editor:
        with tempfile.NamedTemporaryFile(
            mode='w',
            suffix='.txt',
            delete=False,
            encoding='utf-8',
        ) as tmp:
            tmp.write(content)
            tmp_path = Path(tmp.name)

        mtime_before = tmp_path.stat().st_mtime_ns

        try:
            subprocess.run([editor, str(tmp_path)], check=True)
            mtime_after = tmp_path.stat().st_mtime_ns
            raw = tmp_path.read_text(encoding='utf-8')
        finally:
            tmp_path.unlink(missing_ok=True)

        # No save — editor didn't touch the file.
        if mtime_after == mtime_before:
            return None
    else:
        # No editor available — print the draft and read a replacement inline.
        print('\n--- Review (no $EDITOR set) ---')
        print(content)
        print('--- End ---\n')
        print(
            'Press Enter to accept as-is, type a replacement message, '
            'or type "cancel" to abort:'
        )
        user_input = input().strip()
        if user_input.lower() == 'cancel':
            return None
        raw = user_input if user_input else content

    stripped = _strip_comments(raw)
    return stripped if stripped else None


def _strip_comments(text: str) -> str:
    """Remove comment lines (``#``-prefixed) and return stripped result."""
    lines = [line for line in text.splitlines() if not line.startswith('#')]
    return '\n'.join(lines).strip()
