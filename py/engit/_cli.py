"""engit._cli -- Command-line interface for engit.

Subcommands
-----------
tag      Create a semantic version git tag.
release  Create a GitHub release from a tag.
search   Search GitHub repositories.
"""

from __future__ import annotations

import sys
import argparse

from ._exceptions import EngitError
from ._search import ORGS_ENV_VAR


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog='engit',
        description='engit: git and GitHub tooling for envoy bundles.',
    )

    sub = parser.add_subparsers(dest='command', metavar='COMMAND')
    sub.required = True

    # ------------------------------------------------------------------
    # engit tag
    # ------------------------------------------------------------------
    tag_p = sub.add_parser(
        'tag',
        help='Create a semantic version git tag.',
        description=(
            'Create an annotated git tag at HEAD using semantic versioning. '
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
        '--version',
        metavar='VERSION',
        dest='explicit_version',
        help='Explicit version string, e.g. 1.2.3 or v1.2.3. Must be valid SemVer.',
    )

    tag_p.add_argument(
        '--message', '-m',
        metavar='MESSAGE',
        help='Custom tag annotation. Defaults to "Release vMAJOR.MINOR.PATCH".',
    )
    tag_p.add_argument(
        '--push',
        action='store_true',
        help='Push the tag to the remote after creation.',
    )
    tag_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote name to push to (default: origin).',
    )
    tag_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Print the planned tag without creating it.',
    )

    # ------------------------------------------------------------------
    # engit release
    # ------------------------------------------------------------------
    rel_p = sub.add_parser(
        'release',
        help='Create a GitHub release from a tag.',
        description=(
            'Create a GitHub release using the gh CLI. '
            'Automatically aggregates commit messages since the previous tag '
            'into a draft changelog for review before publishing.'
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
        '--push',
        action='store_true',
        help='Push the tag to the remote before creating the release.',
    )
    rel_p.add_argument(
        '--remote',
        default='origin',
        metavar='REMOTE',
        help='Remote name to push to (default: origin).',
    )
    rel_p.add_argument(
        '--yes', '-y',
        action='store_true',
        help='Skip the editor and use the auto-generated release notes unchanged.',
    )
    rel_p.add_argument(
        '--dry-run',
        action='store_true',
        help='Print the planned release without creating it.',
    )

    # ------------------------------------------------------------------
    # engit search
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

    return parser


def main(argv: list[str] | None = None) -> int:
    """Main CLI entry point.

    Args:
        argv: Argument list. Defaults to :data:`sys.argv[1:]`.

    Returns:
        Exit code.

    """
    parser = _build_parser()
    args = parser.parse_args(argv)

    try:
        if args.command == 'tag':
            from ._tag import run_tag
            run_tag(
                bump=args.bump,
                version=args.explicit_version,
                message=args.message,
                push=args.push,
                remote=args.remote,
                dry_run=args.dry_run,
            )

        elif args.command == 'release':
            from ._release import run_release
            run_release(
                tag=args.tag,
                title=args.title,
                draft=args.draft,
                push=args.push,
                remote=args.remote,
                yes=args.yes,
                dry_run=args.dry_run,
            )

        elif args.command == 'search':
            from ._search import run_search
            run_search(
                args.query,
                orgs=args.orgs,
                limit=args.limit,
            )

    except EngitError as exc:
        print(f'Error: {exc}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('\nAborted.', file=sys.stderr)
        return 130

    return 0


if __name__ == '__main__':
    sys.exit(main())
