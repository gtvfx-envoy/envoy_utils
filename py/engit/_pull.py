"""engit pull command — git pull on one or more envoy bundle checkouts.

Resolves bundle IDs via ``ENVOY_BNDL_ROOTS`` and runs ``git pull`` in each
repo using Python's :mod:`subprocess` with ``cwd`` set to the bundle path —
no directory changes to the calling process.

Usage::

    engit pull gt:python
    engit pull gt:python gt:globals
    engit pull *
    engit pull * --rebase --dry-run

"""

from __future__ import annotations

import sys
from pathlib import Path

from ._exceptions import EngitError, GitError
from ._git import isGitRepo
from ._git import pull as git_pull


def _resolveSpecs(specs: list[str]) -> list[tuple[str, Path]]:
    """Resolve bundle specs to ``(bndlid, path)`` pairs.

    Args:
        specs: List of bundle ID strings (e.g. ``['gt:python', 'gt:globals']``)
            or ``['*']`` to discover all bundles from ``ENVOY_BNDL_ROOTS``.

    Returns:
        Ordered list of ``(bndlid, path)`` 2-tuples, one per bundle.

    Raises:
        EngitError: If ``*`` is used and ``ENVOY_BNDL_ROOTS`` is not set, or if
            a named bundle ID cannot be resolved.

    """
    try:
        from envoy._discovery import Bundle, discoverBundlesAuto
        from envoy._exceptions import WrapperError
    except ImportError as exc:
        raise EngitError(
            f"Cannot import envoy bundle discovery: {exc}\nEnsure the envoy package is on sys.path."
        ) from exc

    if specs == ['*']:
        bundles = discoverBundlesAuto()
        if not bundles:
            raise EngitError(
                "No bundles discovered. Is ENVOY_BNDL_ROOTS set and pointing to bundle checkouts?"
            )
        return [(b.bndlid, b.root) for b in bundles]

    pairs: list[tuple[str, Path]] = []
    for spec in specs:
        try:
            bundle = Bundle(spec)
        except WrapperError as exc:
            raise EngitError(str(exc)) from exc
        except ValueError as exc:
            raise EngitError(str(exc)) from exc
        pairs.append((bundle.bndlid, bundle.path))
    return pairs


def runPull(
    specs: list[str],
    *,
    remote: str = 'origin',
    rebase: bool = False,
    dry_run: bool = False,
) -> None:
    """Pull one or more envoy bundle checkouts.

    Resolves each spec to a bundle path and runs ``git pull`` in that
    directory via subprocess (``cwd=<bundle_path>``).  The calling process's
    working directory is never changed.

    Args:
        specs: Bundle IDs (e.g. ``['gt:python']``) or ``['*']`` for all
            bundles discovered from ``ENVOY_BNDL_ROOTS``.
        remote: Remote to pull from. Defaults to ``'origin'``.
        rebase: Pass ``--rebase`` to ``git pull``.
        dry_run: Print what would be pulled without running git.

    Raises:
        EngitError: If bundle resolution fails (e.g. ``ENVOY_BNDL_ROOTS`` not
            set, unknown bundle ID).  In multi-bundle mode per-bundle git
            failures are collected and reported as a summary rather than
            raising immediately.

    """
    bundles = _resolveSpecs(specs)
    multi = len(bundles) > 1

    if dry_run:
        action = f'git pull{" --rebase" if rebase else ""} {remote}'
        if multi:
            print(f'Would pull {len(bundles)} bundle(s) [{action}]:')
            for bndlid, path in bundles:
                print(f'  {bndlid:<20}  {path}')
        else:
            bndlid, path = bundles[0]
            print(f'Would pull {bndlid}  ({path})\n  Command: {action}')
        return

    if multi:
        print(f'Pulling {len(bundles)} bundle(s)...')

    failures: list[tuple[str, str]] = []
    col = max(len(b) for b, _ in bundles) + 2

    for bndlid, path in bundles:
        if not isGitRepo(path):
            msg = 'skipped (not a git repo)'
            if multi:
                print(f'  {bndlid:<{col}} ⚠  {msg}')
            else:
                print(f'{bndlid}: {msg}', file=sys.stderr)
            continue

        if multi:
            print(f'  {bndlid:<{col}}', end='', flush=True)
        else:
            print(f'Pulling {bndlid}  ({path})')

        try:
            output = git_pull(remote=remote, rebase=rebase, cwd=path)
            first_line = output.splitlines()[0] if output.strip() else 'Done.'
            if multi:
                print(f' ✓  {first_line}')
            else:
                print(output if output.strip() else 'Already up to date.')
        except GitError as exc:
            err = str(exc).splitlines()[0]
            if multi:
                print(f' ✗  {err}')
                failures.append((bndlid, str(exc)))
            else:
                raise EngitError(str(exc)) from exc

    if multi:
        succeeded = len(bundles) - len(failures)
        print(f'\nDone. {succeeded} succeeded, {len(failures)} failed.')
        if failures:
            print('  FAILED:')
            for bndlid, err in failures:
                print(f'    {bndlid}: {err.splitlines()[0]}')
