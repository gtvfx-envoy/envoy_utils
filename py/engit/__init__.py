"""
engit -- git and GitHub tooling for envoy bundle development.

Provides semantic version tagging, GitHub release creation, and repository
search, all driven from the command line.

CLI usage::

    engit tag --patch
    engit tag --minor
    engit tag --major
    engit tag --version 1.0.0

    engit release
    engit release --tag v1.2.3 --draft

    engit search pythoncore
    engit search maya --org gtvfx-contrib --org gtvfx

Submodules:
    _semver    -- SemVer dataclass and parsing
    _git       -- git subprocess wrappers
    _github    -- gh CLI wrappers
    _tag       -- tag command logic
    _release   -- release command logic
    _search    -- search command logic
    _cli       -- argument parser and entry point
    _exceptions -- all engit exception classes
"""

from __future__ import annotations

__all__ = [
    # ---- Exceptions ----
    'EngitError',
    'GitError',
    'NotAGitRepoError',
    'NoTagsFoundError',
    'SemVerError',
    'GitHubError',
    'GhCliNotFoundError',

    # ---- Core types ----
    'SemVer',

    # ---- Entry point ----
    'main',
]

from ._exceptions import (
    EngitError,
    GitError,
    NotAGitRepoError,
    NoTagsFoundError,
    SemVerError,
    GitHubError,
    GhCliNotFoundError,
)
from ._semver import SemVer
from ._cli import main
