"""engit publish — Bundle publish command.

Creates a versioned deployment snapshot of a bundle, either as a directory
tree or a zip archive, with git and build artefacts stripped out.

Output layout (folder or inside zip):

    <bundle-name>/
    └── <version>/
        └── <bundle contents>

This structure is designed to be extracted directly into a production install
root so that multiple versions of the same bundle coexist cleanly.

"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import shutil
import zipfile
from datetime import datetime, timezone
from pathlib import Path

from ._exceptions import EngitError, NoTagsFoundError
from ._git import get_latest_semver_tag, is_git_repo


#: Version string used for local test builds (no git tag required).
DEV_VERSION: str = 'dev'

#: Name of the bundle marker file written at the root of every published bundle.
#: Acts as a discovery anchor for ``ENVOY_BNDL_ROOTS`` scanning and carries
#: version metadata consumed by :class:`~envoy._discovery.Bundle`.
BUNDLE_MARKER_FILE: str = '.bundle'

#: Namespace regex — mirrors the one in ``envoy._discovery``.
_NAMESPACE_RE = re.compile(r'^[A-Za-z][A-Za-z0-9_]{1,19}$')

#: Fallback namespace when the parent directory name is not a valid token.
_DEFAULT_NAMESPACE: str = 'gt'

#: Patterns excluded from every publish regardless of bundle type.
#: Matched against each path component (directory names) and file names
#: using ``fnmatch`` glob rules.
DEFAULT_EXCLUDES: frozenset[str] = frozenset({
    '.git',
    '.gitignore',
    '.github',
    'build',
    'dist',
    '.pytest_cache',
    '__pycache__',
    '*.pyc',
    '*.pyo',
    '*.pyd',
})


class PublishError(EngitError):
    """Raised when a bundle publish operation fails."""


# ---------------------------------------------------------------------------
# Version detection
# ---------------------------------------------------------------------------

def detectVersion(bundle_path: Path) -> str:
    """Return the latest semver tag for the bundle's git repository.

    Args:
        bundle_path: Root directory of the bundle.

    Returns:
        Version string including the ``v`` prefix (e.g. ``v1.2.0``).

    Raises:
        PublishError: If the directory is not a git repo or has no semver tags.

    """
    if not is_git_repo(cwd=bundle_path):
        raise PublishError(
            f"'{bundle_path}' is not inside a git repository. "
            f"Use --version dev for a test build without a git tag."
        )

    tag = get_latest_semver_tag(cwd=bundle_path)
    if tag is None:
        raise PublishError(
            f"No semantic version tags found in '{bundle_path}'. "
            f"Create one with 'engit tag' or use --version dev."
        )

    return str(tag)


# ---------------------------------------------------------------------------
# Bundle marker
# ---------------------------------------------------------------------------

def _bundleMarkerData(bndlid: str, bundle_name: str, version: str) -> dict:
    """Return the dict written to the ``.bundle`` marker file.

    Args:
        bndlid: Namespaced bundle identifier (e.g. ``'gt:globals'``).
        bundle_name: Name of the bundle (directory name).
        version: Version string (e.g. ``v1.2.0`` or ``dev``).

    Returns:
        Dict with ``bndlid``, ``name``, ``version``, and ``published`` fields.

    """
    return {
        'bndlid': bndlid,
        'name': bundle_name,
        'version': version,
        'published': datetime.now(timezone.utc).isoformat(),
    }


# ---------------------------------------------------------------------------
# Exclusion helpers
# ---------------------------------------------------------------------------

def _isExcluded(name: str, excludes: frozenset[str]) -> bool:
    """Return True if *name* matches any pattern in *excludes*.

    Args:
        name: A single path component (directory or file name).
        excludes: Set of glob patterns to test against.

    Returns:
        True if the name matches at least one pattern.

    """
    return any(fnmatch.fnmatch(name, pattern) for pattern in excludes)


def _collectFiles(
    bundle_path: Path,
    excludes: frozenset[str],
) -> list[Path]:
    """Collect all files under *bundle_path* that are not excluded.

    Whole directories are pruned early when their name matches *excludes*,
    so their contents are never visited.

    Args:
        bundle_path: Root directory of the bundle.
        excludes: Combined set of default and extra exclude patterns.

    Returns:
        List of absolute :class:`~pathlib.Path` objects for each included file.

    """
    collected: list[Path] = []

    for root_str, dirs, files in os.walk(bundle_path):
        root = Path(root_str)

        # Prune excluded directories in-place (prevents os.walk descent).
        dirs[:] = [d for d in dirs if not _isExcluded(d, excludes)]

        for filename in files:
            if not _isExcluded(filename, excludes):
                collected.append(root / filename)

    return sorted(collected)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def listPublishFiles(
    bundle_path: Path,
    extra_excludes: list[str] | None = None,
) -> list[Path]:
    """Return the list of files that would be included in a publish.

    Useful for ``--dry-run`` previews.

    Args:
        bundle_path: Root directory of the bundle.
        extra_excludes: Additional glob patterns to exclude beyond the defaults.

    Returns:
        Sorted list of absolute paths that would be included.

    """
    excludes = DEFAULT_EXCLUDES | frozenset(extra_excludes or [])
    return _collectFiles(bundle_path.resolve(), excludes)


def bundlePublish(
    bundle_path: Path,
    output_dir: Path,
    version: str,
    *,
    zip_mode: bool = False,
    extra_excludes: list[str] | None = None,
    dry_run: bool = False,
) -> Path:
    """Create a versioned publish of *bundle_path*.

    The output is placed under ``output_dir/<bundle-name>/<version>/``
    (folder mode) or written as ``output_dir/<bundle-name>-<version>.zip``
    with internal path ``<bundle-name>/<version>/...`` (zip mode).

    Args:
        bundle_path: Root directory of the bundle to publish.
        output_dir: Root directory to write the output into.
        version: Version string (e.g. ``v1.2.0`` or ``dev``).
        zip_mode: If ``True``, create a zip archive. If ``False`` (default),
            create a versioned directory tree.
        extra_excludes: Additional glob patterns to exclude beyond the defaults.
        dry_run: If ``True``, print included files and return without writing.

    Returns:
        Path to the created folder or zip file.

    Raises:
        PublishError: If the bundle path does not exist or output cannot be written.

    """
    bundle_path = bundle_path.resolve()

    if not bundle_path.is_dir():
        raise PublishError(f"Bundle path does not exist: '{bundle_path}'")

    bundle_name = bundle_path.name
    parent_name = bundle_path.parent.name
    namespace = parent_name if _NAMESPACE_RE.match(parent_name) else _DEFAULT_NAMESPACE
    bndlid = f"{namespace}:{bundle_name}"
    excludes = DEFAULT_EXCLUDES | frozenset(extra_excludes or [])

    files = _collectFiles(bundle_path, excludes)

    if dry_run:
        print(f"Bundle: {bndlid}  version: {version}")
        print(f"Mode:   {'zip' if zip_mode else 'folder'}")
        print(f"Files that would be included ({len(files)}):")
        for file_path in files:
            rel = file_path.relative_to(bundle_path)
            print(f"  {rel}")
        return output_dir / bundle_name / version

    output_dir = output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    if zip_mode:
        return _buildPublishZip(
            bundle_path, bundle_name, bndlid, version, files, output_dir
        )
    return _buildPublishDir(
        bundle_path, bundle_name, bndlid, version, files, output_dir
    )


def _buildPublishDir(
    bundle_path: Path,
    bundle_name: str,
    bndlid: str,
    version: str,
    files: list[Path],
    output_dir: Path,
) -> Path:
    """Copy bundle files into a versioned directory structure.

    Args:
        bundle_path: Absolute root of the source bundle.
        bundle_name: Name of the bundle (used as the top-level folder).
        bndlid: Namespaced bundle identifier (e.g. ``'gt:globals'``).
        version: Version string.
        files: Absolute paths of files to include.
        output_dir: Root output directory.

    Returns:
        Path to the created versioned directory.

    """
    dest_root = output_dir / bundle_name / version

    if dest_root.exists():
        shutil.rmtree(dest_root)
    dest_root.mkdir(parents=True)

    for src in files:
        rel = src.relative_to(bundle_path)
        dest = dest_root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)

    marker_data = _bundleMarkerData(bndlid, bundle_name, version)
    (dest_root / BUNDLE_MARKER_FILE).write_text(
        json.dumps(marker_data, indent=2), encoding='utf-8'
    )

    return dest_root


def _buildPublishZip(
    bundle_path: Path,
    bundle_name: str,
    bndlid: str,
    version: str,
    files: list[Path],
    output_dir: Path,
) -> Path:
    """Create a zip archive with versioned internal paths.

    Internal zip paths follow ``<bundle-name>/<version>/<relative-path>``.

    Args:
        bundle_path: Absolute root of the source bundle.
        bundle_name: Name of the bundle.
        bndlid: Namespaced bundle identifier (e.g. ``'gt:globals'``).
        version: Version string.
        files: Absolute paths of files to include.
        output_dir: Directory to write the zip file into.

    Returns:
        Path to the created zip file.

    """
    zip_name = f"{bundle_name}-{version}.zip"
    zip_path = output_dir / zip_name

    with zipfile.ZipFile(zip_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
        for src in files:
            rel = src.relative_to(bundle_path)
            arc_name = f"{bundle_name}/{version}/{rel}"
            # Normalise to forward slashes inside the zip (cross-platform).
            arc_name = arc_name.replace('\\', '/')
            zf.write(src, arc_name)

        marker_data = _bundleMarkerData(bndlid, bundle_name, version)
        marker_arc = f"{bundle_name}/{version}/{BUNDLE_MARKER_FILE}"
        zf.writestr(marker_arc, json.dumps(marker_data, indent=2))

    return zip_path
