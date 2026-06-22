"""engit publish — Bundle publish command.

Creates a versioned deployment snapshot of a bundle, either as a directory
tree or a zip archive, with git and build artefacts stripped out.

Output layout (folder or inside zip):

    <repo-segments>/
    └── <version>/
        └── <bundle contents>

The output path is derived by splitting the bundle (repo) name on hyphens so
that each segment becomes its own directory level.  For example, a bundle
named ``gt-ext-python`` is published to ``gt/ext/python/<version>/``.

The ``bndlid`` follows the same rule with colons as separators:
``gt-ext-python`` → ``gt:ext:python``.

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
from ._git import getLatestSemverTag, isGitRepo


#: Version string used for local test builds (no git tag required).
DEV_VERSION: str = 'dev'

#: Name of the bundle marker file written at the root of every published bundle.
#: Acts as a discovery anchor for ``ENVOY_BNDL_ROOTS`` scanning and carries
#: version metadata consumed by :class:`~envoy._discovery.Bundle`.
BUNDLE_MARKER_FILE: str = '.bundle'

#: Name of the optional artifact-sources config file inside the bundle config dir.
BUNDLE_ARTIFACTS_FILE: str = 'bundle-artifacts.json'

#: Name of the per-bundle configuration directory (mirrors ``envoy.BUNDLE_ENV_DIR``).
BUNDLE_ENV_DIR: str = '.envoy'

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
    if not isGitRepo(cwd=bundle_path):
        raise PublishError(
            f"'{bundle_path}' is not inside a git repository. "
            f"Use --version dev for a test build without a git tag."
        )

    tag = getLatestSemverTag(cwd=bundle_path)
    if tag is None:
        raise PublishError(
            f"No semantic version tags found in '{bundle_path}'. "
            f"Create one with 'engit tag' or use --version dev."
        )

    return str(tag)


# ---------------------------------------------------------------------------
# Bundle id helpers
# ---------------------------------------------------------------------------

def _bndlidFromName(bundle_name: str) -> str:
    """Convert a bundle directory name to a ``bndlid``.

    Each hyphen-separated segment of *bundle_name* becomes a colon-separated
    component of the id.  For example ``gt-ext-python`` → ``gt:ext:python``.

    Args:
        bundle_name: Directory name of the bundle (typically the repo name).

    Returns:
        Colon-separated bundle identifier string.

    """
    return bundle_name.replace('-', ':')


def _publishPath(bundle_name: str) -> Path:
    """Convert a bundle name to its publish directory path.

    Each hyphen-separated segment becomes its own directory level so that
    ``gt-ext-python`` produces ``Path('gt/ext/python')``.

    Args:
        bundle_name: Directory name of the bundle.

    Returns:
        :class:`~pathlib.Path` with one component per hyphen-separated segment.

    """
    return Path(*bundle_name.split('-'))


# ---------------------------------------------------------------------------
# Bundle marker
# ---------------------------------------------------------------------------

def _bundleMarkerData(bndlid: str, bndl_name: str, version: str) -> dict:
    """Return the dict written to the ``.bundle`` marker file.

    Args:
        bndlid: Namespaced bundle identifier (e.g. ``'gt:ext:python'``).
        bndl_name: Logical bundle name without the namespace prefix
            (e.g. ``'ext:python'``).
        version: Version string (e.g. ``v1.2.0`` or ``dev``).

    Returns:
        Dict with ``bndlid``, ``name``, ``version``, and ``published`` fields.

    """
    return {
        'bndlid': bndlid,
        'name': bndl_name,
        'version': version,
        'published': datetime.now(timezone.utc).isoformat(),
    }


# ---------------------------------------------------------------------------
# Asset source helpers
# ---------------------------------------------------------------------------

def _baseVersion(version: str) -> str:
    """Strip the ``-envoy.<int>`` iteration suffix from a version string.

    For external package bundles the git tag includes an envoy release suffix
    (e.g. ``3.11.9-envoy.1``).  The base version (``3.11.9``) is used as the
    key when resolving asset store paths.

    Args:
        version: Full version string, optionally with ``-envoy.<int>`` suffix.

    Returns:
        Version string with the suffix removed, or the original string if no
        suffix is present.

    """
    return re.sub(r'-envoy\.\d+$', '', version)


def _resolveAssetTokens(value: str, version: str) -> str:
    """Expand ``${VAR}`` tokens in *value* against environment variables.

    Resolution order for each ``${VAR}`` reference:

    1. Built-in tokens — ``VERSION`` and ``BASE_VERSION`` — which are derived
       from the publish version string and are always available regardless of
       the environment.
    2. OS environment variables (``os.environ``).

    Built-ins take precedence so that ``${VERSION}`` and ``${BASE_VERSION}``
    always reflect the current publish rather than a stale environment value.
    Unknown tokens that have no matching env var are left unexpanded.

    Args:
        value: String that may contain ``${VAR}`` references.
        version: Full publish version string (e.g. ``3.11.9-envoy.1``).

    Returns:
        String with all resolvable tokens replaced.

    """
    builtins = {
        'VERSION': version,
        'BASE_VERSION': _baseVersion(version),
    }

    def _replace(match: re.Match) -> str:
        name = match.group(1)
        if name in builtins:
            return builtins[name]
        return os.environ.get(name, match.group(0))

    return re.sub(r'\$\{([^}]+)\}', _replace, value)


def _loadBundleArtifacts(bundle_path: Path, version: str) -> list[dict]:
    """Read ``.envoy/bundle-artifacts.json`` and resolve tokens in source paths.

    Returns an empty list when the file is absent or cannot be parsed, so
    callers can treat the no-artifacts case uniformly.

    Token expansion in ``source`` paths supports any ``${VAR}`` reference.
    Built-in tokens are resolved first:

    * ``${VERSION}`` — the full version string (e.g. ``3.11.9-envoy.1``).
    * ``${BASE_VERSION}`` — the version with the ``-envoy.<int>`` suffix
      stripped (e.g. ``3.11.9``).

    All other ``${VAR}`` tokens are resolved against OS environment variables,
    making it straightforward to parameterise the artifact store root::

        "${ENVOY_STUDIO_ARTIFACTS}/ext/python/3.11.9"

    Tokens that match neither a built-in nor an environment variable are left
    unexpanded so misconfiguration is visible rather than silently wrong.

    Args:
        bundle_path: Root directory of the bundle.
        version: Version string used to resolve built-in tokens.

    Returns:
        List of artifact entry dicts with tokens in ``source`` resolved.

    """
    artifacts_file = bundle_path / BUNDLE_ENV_DIR / BUNDLE_ARTIFACTS_FILE
    if not artifacts_file.is_file():
        return []
    try:
        data = json.loads(artifacts_file.read_text(encoding='utf-8'))
        resolved = []
        for artifact in data.get('artifacts', []):
            entry = dict(artifact)
            entry['source'] = _resolveAssetTokens(entry['source'], version)
            resolved.append(entry)
        return resolved
    except (OSError, json.JSONDecodeError, KeyError, TypeError):
        return []


def _collectArtifactFiles(
    artifact_source: Path,
    excludes: frozenset[str],
) -> list[tuple[Path, Path]]:
    """Collect files from *artifact_source*, returning ``(src, rel)`` pairs.

    Applies the same exclusion rules as :func:`_collectFiles`.  *rel* is
    relative to *artifact_source* so the caller can map it to any destination
    inside the bundle.

    Args:
        artifact_source: Root directory of the artifact source.
        excludes: Combined set of exclusion patterns.

    Returns:
        Sorted list of ``(absolute_src, relative_path)`` tuples.

    """
    collected: list[tuple[Path, Path]] = []

    for root_str, dirs, files in os.walk(artifact_source):
        root = Path(root_str)
        dirs[:] = [d for d in dirs if not _isExcluded(d, excludes)]
        for filename in files:
            if not _isExcluded(filename, excludes):
                src = root / filename
                collected.append((src, src.relative_to(artifact_source)))

    return sorted(collected, key=lambda x: x[1])


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
    version: str = DEV_VERSION,
    extra_excludes: list[str] | None = None,
) -> list[tuple[Path, Path]]:
    """Return the list of files that would be included in a publish.

    Useful for ``--dry-run`` previews.  Each tuple is
    ``(absolute_src, relative_dest)`` where *relative_dest* is the path
    inside the versioned bundle root.

    Args:
        bundle_path: Root directory of the bundle.
        version: Version string used to resolve tokens in asset source paths.
        extra_excludes: Additional glob patterns to exclude beyond the defaults.

    Returns:
        Sorted list of ``(absolute_src, relative_dest)`` tuples.

    """
    bundle_path = bundle_path.resolve()
    excludes = DEFAULT_EXCLUDES | frozenset(extra_excludes or [])

    files: list[tuple[Path, Path]] = [
        (f, f.relative_to(bundle_path))
        for f in _collectFiles(bundle_path, excludes)
    ]

    for artifact in _loadBundleArtifacts(bundle_path, version):
        src_root = Path(artifact['source'])
        dest_rel = Path(artifact.get('dest', '.'))
        if not src_root.is_dir():
            continue
        for src, rel in _collectArtifactFiles(src_root, excludes):
            rel_in_bundle = rel if dest_rel == Path('.') else dest_rel / rel
            files.append((src, rel_in_bundle))

    return files


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

    The output path is derived by splitting the bundle name on hyphens:
    ``gt-ext-python`` → ``gt/ext/python/<version>/`` (folder mode) or
    ``gt-ext-python-<version>.zip`` with internal paths
    ``gt/ext/python/<version>/...`` (zip mode).

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

    bundle_name  = bundle_path.name
    bndlid       = _bndlidFromName(bundle_name)
    bndl_name    = bndlid.split(':', 1)[1]
    pub_path     = _publishPath(bundle_name)
    excludes     = DEFAULT_EXCLUDES | frozenset(extra_excludes or [])

    files: list[tuple[Path, Path]] = [
        (f, f.relative_to(bundle_path))
        for f in _collectFiles(bundle_path, excludes)
    ]

    for artifact in _loadBundleArtifacts(bundle_path, version):
        src_root = Path(artifact['source'])
        dest_rel = Path(artifact.get('dest', '.'))
        if not src_root.is_dir():
            continue
        for src, rel in _collectArtifactFiles(src_root, excludes):
            rel_in_bundle = rel if dest_rel == Path('.') else dest_rel / rel
            files.append((src, rel_in_bundle))

    if dry_run:
        print(f"Bundle: {bndlid}  version: {version}")
        print(f"Mode:   {'zip' if zip_mode else 'folder'}")
        print(f"Files that would be included ({len(files)}):")
        for _, rel in files:
            print(f"  {rel}")
        return output_dir / pub_path / version

    output_dir = output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    if zip_mode:
        return _buildPublishZip(
            bundle_name, bndlid, bndl_name, version, files, output_dir
        )
    return _buildPublishDir(
        bundle_name, bndlid, bndl_name, version, files, output_dir
    )


def _buildPublishDir(
    bundle_name: str,
    bndlid: str,
    bndl_name: str,
    version: str,
    files: list[tuple[Path, Path]],
    output_dir: Path,
) -> Path:
    """Copy bundle files into a versioned directory structure.

    Args:
        bundle_name: Directory name of the bundle (e.g. ``'gt-ext-python'``).
        bndlid: Namespaced bundle identifier (e.g. ``'gt:ext:python'``).
        bndl_name: Logical bundle name without the namespace prefix.
        version: Version string.
        files: List of ``(absolute_src, relative_dest)`` tuples.
        output_dir: Root output directory.

    Returns:
        Path to the created versioned directory.

    """
    dest_root = output_dir / _publishPath(bundle_name) / version

    if dest_root.exists():
        shutil.rmtree(dest_root)
    dest_root.mkdir(parents=True)

    for src, rel in files:
        dest = dest_root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)

    marker_data = _bundleMarkerData(bndlid, bndl_name, version)
    (dest_root / BUNDLE_MARKER_FILE).write_text(
        json.dumps(marker_data, indent=2), encoding='utf-8'
    )

    return dest_root


def _buildPublishZip(
    bundle_name: str,
    bndlid: str,
    bndl_name: str,
    version: str,
    files: list[tuple[Path, Path]],
    output_dir: Path,
) -> Path:
    """Create a zip archive with versioned internal paths.

    Internal zip paths follow ``<publish-path>/<version>/<relative-path>``
    where the publish path is derived by replacing hyphens in *bundle_name*
    with forward slashes (e.g. ``gt-ext-python`` → ``gt/ext/python``).

    Args:
        bundle_name: Directory name of the bundle (e.g. ``'gt-ext-python'``).
        bndlid: Namespaced bundle identifier (e.g. ``'gt:ext:python'``).
        bndl_name: Logical bundle name without the namespace prefix.
        version: Version string.
        files: List of ``(absolute_src, relative_dest)`` tuples.
        output_dir: Directory to write the zip file into.

    Returns:
        Path to the created zip file.

    """
    pub_path_str = bundle_name.replace('-', '/')
    zip_name = f"{bundle_name}-{version}.zip"
    zip_path = output_dir / zip_name

    with zipfile.ZipFile(zip_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
        for src, rel in files:
            arc_name = f"{pub_path_str}/{version}/{rel.as_posix()}"
            zf.write(src, arc_name)

        marker_data = _bundleMarkerData(bndlid, bndl_name, version)
        marker_arc = f"{pub_path_str}/{version}/{BUNDLE_MARKER_FILE}"
        zf.writestr(marker_arc, json.dumps(marker_data, indent=2))

    return zip_path
