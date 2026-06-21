"""Semantic version parsing, validation, and incrementing.

Handles the ``vMAJOR.MINOR.PATCH[-PRERELEASE]`` tag format used by engit.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from ._exceptions import SemVerError


# Matches optional leading 'v' followed by MAJOR.MINOR.PATCH and an optional
# prerelease suffix of the form -LABEL or -LABEL.N (e.g. -alpha, -alpha.3).
_SEMVER_RE = re.compile(
    r'^v?(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)'
    r'(?:-(?P<prerelease>[a-zA-Z][a-zA-Z0-9]*(?:\.\d+)?))?$'
)


@dataclass(frozen=True)
class SemVer:
    """An immutable semantic version with optional prerelease identifier.

    Attributes:
        major: Breaking change increment.
        minor: Backwards-compatible feature increment.
        patch: Backwards-compatible bug-fix increment.
        prerelease: Optional prerelease identifier such as ``'alpha'`` or
            ``'alpha.3'``.  ``None`` for a stable release.

    Example::

        v = SemVer(1, 2, 3)
        print(v.toTag())                  # 'v1.2.3'
        print(v.bumpMinor().toTag())     # 'v1.3.0'

        pre = SemVer(1, 2, 3, 'alpha.2')
        print(pre.toTag())                # 'v1.2.3-alpha.2'
        print(pre.prereleaseLabel)        # 'alpha'
        print(pre.prereleaseNumber)       # 2

    """

    major: int
    minor: int
    patch: int
    prerelease: str | None = None

    # ------------------------------------------------------------------
    # Constructors
    # ------------------------------------------------------------------

    @classmethod
    def parse(cls, value: str) -> 'SemVer':
        """Parse a version string, with or without a leading ``v``.

        Supports plain ``MAJOR.MINOR.PATCH`` and prerelease suffixes of the
        form ``-LABEL`` or ``-LABEL.N`` (e.g. ``1.2.3-alpha``,
        ``v0.0.1-alpha.3``).

        Args:
            value: Version string to parse.

        Returns:
            A :class:`SemVer` instance.

        Raises:
            ~._exceptions.SemVerError: If *value* does not match the expected
                pattern.

        """
        m = _SEMVER_RE.match(value.strip())
        if not m:
            raise SemVerError(
                f"'{value}' is not a valid semantic version. "
                "Expected MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-LABEL[.N] "
                "(e.g. 1.2.3, v1.2.3, 1.2.3-alpha, v0.0.1-alpha.3)."
            )
        return cls(
            major=int(m.group('major')),
            minor=int(m.group('minor')),
            patch=int(m.group('patch')),
            prerelease=m.group('prerelease'),
        )

    # ------------------------------------------------------------------
    # Prerelease introspection
    # ------------------------------------------------------------------

    @property
    def prereleaseLabel(self) -> str | None:
        """Return the label part of the prerelease identifier, or ``None``.

        For ``'alpha.3'`` returns ``'alpha'``; for ``'alpha'`` returns
        ``'alpha'``; for a stable release returns ``None``.

        """
        if self.prerelease is None:
            return None
        return self.prerelease.split('.')[0]

    @property
    def prereleaseNumber(self) -> int | None:
        """Return the numeric suffix of the prerelease identifier, or ``None``.

        For ``'alpha.3'`` returns ``3``; for ``'alpha'`` (no number) returns
        ``None``; for a stable release returns ``None``.

        """
        if self.prerelease is None:
            return None
        parts = self.prerelease.split('.')
        if len(parts) < 2:
            return None
        try:
            return int(parts[1])
        except ValueError:
            return None

    # ------------------------------------------------------------------
    # Increment helpers
    # ------------------------------------------------------------------

    def bumpMajor(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *major* incremented.

        Resets *minor*, *patch*, and *prerelease* to their zero/None defaults.

        """
        return SemVer(self.major + 1, 0, 0)

    def bumpMinor(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *minor* incremented.

        Resets *patch* and *prerelease* to their zero/None defaults.

        """
        return SemVer(self.major, self.minor + 1, 0)

    def bumpPatch(self) -> 'SemVer':
        """Return a new :class:`SemVer` with *patch* incremented.

        Clears *prerelease* — bump flags always produce stable releases.

        """
        return SemVer(self.major, self.minor, self.patch + 1)

    # ------------------------------------------------------------------
    # Rendering
    # ------------------------------------------------------------------

    def toTag(self) -> str:
        """Return the version formatted as a git tag string.

        Stable:    ``'vMAJOR.MINOR.PATCH'``
        Prerelease: ``'vMAJOR.MINOR.PATCH-PRERELEASE'``

        """
        base = f'v{self.major}.{self.minor}.{self.patch}'
        if self.prerelease:
            return f'{base}-{self.prerelease}'
        return base

    def __str__(self) -> str:
        base = f'{self.major}.{self.minor}.{self.patch}'
        if self.prerelease:
            return f'{base}-{self.prerelease}'
        return base
