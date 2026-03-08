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
        '--version',
        metavar='VERSION',
        dest='explicit_version',
        help='Explicit version string, e.g. 1.2.3 or v1.2.3. Must be valid SemVer.',
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

    # ------------------------------------------------------------------
    # engit release
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

    # ------------------------------------------------------------------
    # engit status
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

    # ------------------------------------------------------------------
    # engit changelog
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

    # ------------------------------------------------------------------
    # engit cleanup
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

    # ------------------------------------------------------------------
    # engit web
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
                print_only=args.print_only,
                dry_run=args.dry_run,
            )

        elif args.command == 'release':
            from ._release import run_release
            run_release(
                tag=args.tag,
                title=args.title,
                draft=args.draft,
                remote=args.remote,
                print_only=args.print_only,
                dry_run=args.dry_run,
            )

        elif args.command == 'search':
            from ._search import run_search
            run_search(
                args.query,
                orgs=args.orgs,
                limit=args.limit,
            )

        elif args.command == 'status':
            from ._status import run_status
            run_status(remote=args.remote)

        elif args.command == 'changelog':
            from ._changelog import run_changelog
            run_changelog(tag=args.tag)

        elif args.command == 'cleanup':
            from ._cleanup import run_cleanup
            run_cleanup(remote=args.remote, noop=args.noop)

        elif args.command == 'web':
            from ._web import run_web
            run_web(branch=args.branch, remote=args.remote)

    except EngitError as exc:
        print(f'Error: {exc}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('\nAborted.', file=sys.stderr)
        return 130

    return 0


if __name__ == '__main__':
    sys.exit(main())
