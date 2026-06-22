"""engit._cli -- Command-line interface for engit.

Subcommands
-----------
tag             Create a semantic version git tag.
release         Create a GitHub release from a tag.
publish         Create a versioned publish of a bundle (folder or zip).
publish-config  Publish a bundles config file to a named config slot.
search          Search GitHub repositories.
"""

from __future__ import annotations

import sys
import argparse
from pathlib import Path

from ._exceptions import EngitError
from ._search import ORGS_ENV_VAR


def _buildParser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog='engit',
        description='engit: git and GitHub tooling for envoy bundles.',
    )

    sub = parser.add_subparsers(dest='command', metavar='COMMAND')
    sub.required = True

    # ------------------------------------------------------------------
    # region: engit tag
    # ------------------------------------------------------------------
    tag_p = sub.add_parser(
        'tag',
        help='Create a semantic version git tag.',
        description=(
            'Create an annotated git tag at HEAD using semantic versioning. '
            'The editor is pre-populated with commit bullets for changelist '
            'curation; the saved text becomes the tag annotation used by '
            'engit release. '
            'Provide one of --major / --minor / --patch to increment the current '
            'latest tag, or --version to supply an explicit version.'
        ),
    )

    bump_group = tag_p.add_mutually_exclusive_group(required=True)
    bump_group.add_argument(
        '--major',
        dest='bump',
        action='store_const',
        const='major',
        help='Increment the major version component (resets minor and patch to 0).',
    )
    bump_group.add_argument(
        '--minor',
        dest='bump',
        action='store_const',
        const='minor',
        help='Increment the minor version component (resets patch to 0).',
    )
    bump_group.add_argument(
        '--patch',
        dest='bump',
        action='store_const',
        const='patch',
        help='Increment the patch version component.',
    )
    bump_group.add_argument(
        '--version', '-v',
        metavar='VERSION',
        dest='explicit_version',
        type=str,
        help=(
            'Explicit version string. Supports stable releases (e.g. 1.2.3, v1.2.3) '
            'and prerelease suffixes (e.g. 1.2.3-alpha, v0.0.1-beta). '
            'Omit the sequence number to auto-detect the next one — '
            '--version 1.2.3-alpha creates v1.2.3-alpha.1, or v1.2.3-alpha.4 '
            'if v1.2.3-alpha.1 through .3 already exist. '
            'Supply the number explicitly (e.g. 1.2.3-alpha.2) to use it as-is.'
        ),
    )
    tag_p.add_argument(
        '--message', '-m',
        metavar='MESSAGE',
        help=(
            'Supply the tag annotation directly, skipping the editor. '
            'Defaults to "Release vMAJOR.MINOR.PATCH".'
        ),
    )
    tag_p.add_argument(
        '--print', '-p',
        dest='print_only',
        action='store_true',
        help='Print the computed next version without creating a tag.',
    )
    tag_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Print the planned tag without creating it.',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit release
    # ------------------------------------------------------------------
    rel_p = sub.add_parser(
        'release',
        help='Create a GitHub release from a tag.',
        description=(
            'Create a GitHub release using the gh CLI. '
            'Uses the selected tag annotation (curated in engit tag) as '
            'release notes, with a legacy fallback draft for older tags.'
        ),
    )

    rel_p.add_argument(
        '--tag',
        metavar='TAG',
        help=(
            'Tag to release (e.g. v1.2.3). '
            'Defaults to the most recent semantic version tag.'
        ),
    )
    rel_p.add_argument(
        '--title',
        metavar='TITLE',
        help='Release title. Defaults to the tag string.',
    )
    rel_p.add_argument(
        '--draft',
        action='store_true',
        help='Create the release as a draft (not yet published).',
    )
    rel_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote name to push to (default: origin).',
    )
    rel_p.add_argument(
        '--generate-notes',
        dest='generate_notes',
        action='store_true',
        help=(
            "Append GitHub auto-generated \"What's Changed\" notes from merged "
            'PRs to the release body.'
        ),
    )

    rel_p.add_argument(
        '--print', '-p',
        dest='print_only',
        action='store_true',
        help='Print the resolved release notes without pushing or publishing.',
    )
    rel_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Print the planned release without creating it.',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit status
    # ------------------------------------------------------------------
    status_p = sub.add_parser(
        'status',
        help='Show repository status.',
        description=(
            'Display the current branch, ahead/behind the remote, '
            'last semver tag, and most recent commit.'
        ),
    )
    status_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote name for ahead/behind comparison (default: origin).',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit changelog
    # ------------------------------------------------------------------
    changelog_p = sub.add_parser(
        'changelog',
        help='Generate a changelog from GitHub releases.',
        description=(
            'Fetch published GitHub releases, sort by semantic version, '
            'and print a formatted changelog.'
        ),
    )
    changelog_p.add_argument(
        '--tag',
        metavar='TAG',
        help='Show only the release for this tag.',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit cleanup
    # ------------------------------------------------------------------
    cleanup_p = sub.add_parser(
        'cleanup',
        help='Clean up merged and stale local branches.',
        description=(
            'Prunes stale remote-tracking refs, deletes merged branches, '
            'and deletes branches whose remote has been deleted.'
        ),
    )
    cleanup_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote name to prune (default: origin).',
    )
    cleanup_p.add_argument(
        '--noop',
        action='store_true',
        help="Print what would be deleted without actually deleting anything.",
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit web
    # ------------------------------------------------------------------
    web_p = sub.add_parser(
        'web',
        help='Open the repository on GitHub in a browser.',
        description=(
            'Resolves the remote URL and opens the repository '
            '(or a specific branch/tag) in the default web browser.'
        ),
    )
    web_p.add_argument(
        '--branch', '-b',
        metavar='BRANCH',
        help='Branch or tag to view. Defaults to the current branch.',
    )
    web_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote whose URL is opened (default: origin).',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit pull
    # ------------------------------------------------------------------
    pull_p = sub.add_parser(
        'pull',
        help='Pull one or more envoy bundle checkouts.',
        description=(
            'Run git pull on one or more envoy bundle checkouts by bundle ID. '
            'Bundle paths are resolved from ENVOY_BNDL_ROOTS. '
            'Use * to pull all discovered bundles.'
        ),
    )
    pull_p.add_argument(
        'bundles',
        nargs='+',
        metavar='BUNDLE',
        help=(
            'Bundle ID to pull (e.g. gt:python), or * to pull all bundles '
            'discovered via ENVOY_BNDL_ROOTS. Multiple IDs may be supplied.'
        ),
    )
    pull_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote to pull from (default: origin).',
    )
    pull_p.add_argument(
        '--rebase',
        action='store_true',
        help='Pass --rebase to git pull (rebase local commits onto fetched branch).',
    )
    pull_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Print what would be pulled without running git.',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit search
    # ------------------------------------------------------------------
    search_p = sub.add_parser(
        'search',
        help='Search GitHub repositories.',
        description=(
            'Search GitHub for repositories matching a query string. '
            f'Default organisations are read from the {ORGS_ENV_VAR} environment '
            'variable (semicolon-separated). Use --org to override.'
        ),
    )

    search_p.add_argument(
        'query',
        help='Search query string.',
    )
    search_p.add_argument(
        '--org',
        dest='orgs',
        action='append',
        metavar='ORG',
        help=(
            'GitHub organisation to search. '
            'May be specified multiple times. '
            f'Overrides {ORGS_ENV_VAR}.'
        ),
    )
    search_p.add_argument(
        '--limit',
        type=int,
        default=20,
        metavar='N',
        help='Maximum results per organisation (default: 20).',
    )
    # endregion

    # ------------------------------------------------------------------
    # region: engit publish
    # ------------------------------------------------------------------
    pub_p = sub.add_parser(
        'publish',
        help='Create a versioned publish of a bundle.',
        description=(
            'Copy a bundle into a clean versioned output directory or zip archive, '
            'stripping git and build artefacts. '
            'Output layout: <output>/<bundle-name>/<version>/  '
            'or, with --zip: <output>/<bundle-name>-<version>.zip '
            '(internal paths: <bundle-name>/<version>/...).'
        ),
    )
    pub_p.add_argument(
        'path',
        nargs='?',
        default=None,
        metavar='PATH',
        help=(
            'Bundle root directory or bundle ID (e.g. gt:globals). '
            'Defaults to the current directory.'
        ),
    )
    pub_p.add_argument(
        '--output', '-o',
        type=Path,
        default=None,
        metavar='DIR',
        help='Root directory to write the output into. Defaults to the current directory.',
    )
    pub_p.add_argument(
        '--version', '-v',
        type=str,
        default=None,
        metavar='VERSION',
        help=(
            'Explicit version string (e.g. v1.2.0). '
            'Defaults to the latest semver git tag. '
            'Use "dev" to create a test build without requiring a git tag.'
        ),
    )
    pub_p.add_argument(
        '--exclude',
        action='append',
        default=[],
        metavar='PATTERN',
        help=(
            'Additional glob pattern to exclude (e.g. "*.spec"). '
            'May be specified multiple times. '
            'Added on top of the default exclusions (.git, .github, build, dist, etc.).'
        ),
    )
    pub_p.add_argument(
        '--zip',
        action='store_true',
        help='Create a zip archive instead of a versioned directory.',
    )
    pub_p.add_argument(
        '--dry-run',
        action='store_true',
        help='List the files that would be included without writing any output.',
    )

    # endregion

    # ------------------------------------------------------------------
    # region: engit publish-config
    # ------------------------------------------------------------------
    pcfg_p = sub.add_parser(
        'publish-config',
        help='Publish a bundles config file to a named config slot.',
        description=(
            'Copy a bundles-config JSON file into a versioned named slot under '
            'a config root directory, and update the "latest" pointer. '
            'Published configs can be referenced by name when setting '
            'the envoy bundles_config preference '
            '(e.g. envoy --set-config bundles_config=studio).'
        ),
    )
    pcfg_p.add_argument(
        'name',
        metavar='NAME',
        help='Named config slot (e.g. "studio", "dev", "production").',
    )
    pcfg_p.add_argument(
        'source',
        type=Path,
        metavar='SOURCE',
        help='Path to the bundles-config JSON file to publish.',
    )
    pcfg_p.add_argument(
        '--cfg-root', '-r',
        type=Path,
        default=None,
        metavar='DIR',
        help=(
            'Root directory to publish into.  '
            'Defaults to the first directory in ENVOY_CFG_ROOTS.'
        ),
    )
    pcfg_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Show what would be written without writing anything.',
    )

    # endregion

    return parser


def main(argv: list[str] | None = None) -> int:
    """Main CLI entry point.

    Args:
        argv: Argument list. Defaults to :data:`sys.argv[1:]`.

    Returns:
        Exit code.

    """
    parser = _buildParser()
    args = parser.parse_args(argv)

    try:
        if args.command == 'tag':
            from ._tag import runTag
            runTag(
                bump=args.bump,
                version=args.explicit_version,
                message=args.message,
                print_only=args.print_only,
                dry_run=args.dry_run,
            )

        elif args.command == 'release':
            from ._release import runRelease
            runRelease(
                tag=args.tag,
                title=args.title,
                draft=args.draft,
                remote=args.remote,
                print_only=args.print_only,
                dry_run=args.dry_run,
                generate_notes=args.generate_notes,
            )

        elif args.command == 'pull':
            from ._pull import runPull
            runPull(
                args.bundles,
                remote=args.remote,
                rebase=args.rebase,
                dry_run=args.dry_run,
            )

        elif args.command == 'search':
            from ._search import runSearch
            runSearch(
                args.query,
                orgs=args.orgs,
                limit=args.limit,
            )

        elif args.command == 'status':
            from ._status import runStatus
            runStatus(remote=args.remote)

        elif args.command == 'changelog':
            from ._changelog import runChangelog
            runChangelog(tag=args.tag)

        elif args.command == 'cleanup':
            from ._cleanup import runCleanup
            runCleanup(remote=args.remote, noop=args.noop)

        elif args.command == 'web':
            from ._web import runWeb
            runWeb(branch=args.branch, remote=args.remote)

        elif args.command == 'publish':
            from ._publish import bundlePublish, detectVersion, PublishError

            # Resolve bundle path (path argument or cwd).
            if args.path is None:
                bundle_path = Path.cwd()
            else:
                bundle_path = Path(args.path)

            # Resolve version: explicit arg wins; otherwise detect from git tags.
            if args.version:
                version = args.version
            else:
                version = detectVersion(bundle_path)

            output_dir = Path(args.output) if args.output else Path.cwd()

            result = bundlePublish(
                bundle_path=bundle_path,
                output_dir=output_dir,
                version=version,
                zip_mode=args.zip,
                extra_excludes=args.exclude or [],
                dry_run=args.dry_run,
            )

            if not args.dry_run:
                label = 'zip' if args.zip else 'folder'
                print(f'Published {label}: {result}')

        elif args.command == 'publish-config':
            import os
            from envoy._config_registry import publishConfig, CFG_ROOTS_VAR, _cfgRoots

            cfg_root = args.cfg_root
            if cfg_root is None:
                roots = _cfgRoots()
                if not roots:
                    raise EngitError(
                        f"No --cfg-root specified and {CFG_ROOTS_VAR} is not set."
                    )
                cfg_root = roots[0]

            result = publishConfig(
                cfg_root=cfg_root,
                name=args.name,
                source_path=args.source,
                dry_run=args.dry_run,
            )

            if not args.dry_run:
                print(f"Published config: {args.name}")
                print(f"  Version: {result.stem}")
                print(f"  Path:    {result}")

    except EngitError as exc:
        print(f'Error: {exc}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('\nAborted.', file=sys.stderr)
        return 130

    return 0


if __name__ == '__main__':
    sys.exit(main())
