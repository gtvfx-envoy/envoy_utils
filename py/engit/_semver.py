"""Semantic version parsing, validation, and incrementing.

Handles the ``vMAJOR.MINOR.PATCH`` tag format used by engit.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from ._exceptions import SemVerError


# Matches optional leading 'v' followed by MAJOR.MINOR.PATCH.
# Pre-release and build metadata are intentionally excluded — engit
# tags use plain semver triples only.
_SEMVER_RE = re.compile(r'^v?(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)$')


@dataclass(frozen=True, order=True)
class SemVer:
    """An immutable semantic version triple.

    Attributes:
        major: Breaking change increment.
        minor: Backwards-compatible feature increment.
        patch: Backwards-compatible bug-fix increment.

    Example::

        v = SemVer(1, 2, 3)
        print(v.to_tag())   # 'v1.2.3'
        print(v.bump_minor().to_tag())  # 'v1.3.0'

    """

    major: int
    minor: int
    patch: int

    # ------------------------------------------------------------------
    # Constructors
    # ------------------------------------------------------------------

    @classmethod
    def parse(cls, value: str) -> 'SemVer':
        """Parse a version string, with or without a leading ``v``.

        Args:
            value: Version string such as ``'1.2.3'`` or ``'v1.2.3'``.

        Returns:
            A :class:`SemVer` instance.

        Raises:
            ~._exceptions.SemVerError: If *value* does not match the
                expected ``MAJOR.MINOR.PATCH`` pattern.

        """
        m = _SEMVER_RE.match(value.strip())
        if not m:
            raise SemVerError(
                f"'{value}' is not a valid semantic version. "
                "Expected MAJOR.MINOR.PATCH (e.g. 1.2.3 or v1.2.3)."
            )
        return cls(
            major=int(m.group('major')),
            minor=int(m.group('minor')),
            patch=int(m.group('patch')),
        )

    # ------------------------------------------------------------------
    # Increment helpers
    # ------------------------------------------------------------------

    def bump_major(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *major* incremented.

        Resets *minor* and *patch* to zero.

        """
        return SemVer(self.major + 1, 0, 0)

    def bump_minor(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *minor* incremented.

        Resets *patch* to zero.

        """
        return SemVer(self.major, self.minor + 1, 0)

    def bump_patch(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *patch* incremented."""
        return SemVer(self.major, self.minor, self.patch + 1)

    # ------------------------------------------------------------------
    # Rendering
    # ------------------------------------------------------------------

    def to_tag(self) -> str:
        """Return the version formatted as a git tag string (``vMAJOR.MINOR.PATCH``)."""
        return f'v{self.major}.{self.minor}.{self.patch}'

    def __str__(self) -> str:
        return f'{self.major}.{self.minor}.{self.patch}'
