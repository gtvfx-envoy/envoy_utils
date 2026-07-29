"""Build the native Envoy Utils command-line tools."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RUST_ROOT = REPO_ROOT / "rust"


def parseArguments() -> argparse.Namespace:
    """Parse build arguments.

    Returns:
        Parsed command-line options.

    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--debug",
        action="store_true",
        help="Build the native binaries with Cargo's debug profile.",
    )
    parser.add_argument(
        "--target",
        help="Optional Rust target triple for cross-compilation.",
    )
    return parser.parse_args()


def main() -> int:
    """Build the Rust workspace.

    Returns:
        Process exit status.

    """
    options = parseArguments()
    arguments = ["cargo", "build", "--workspace"]
    if not options.debug:
        arguments.append("--release")
    if options.target:
        arguments.extend(["--target", options.target])

    print(f"+ {' '.join(arguments)}", flush=True)
    try:
        subprocess.run(arguments, cwd=RUST_ROOT, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"Build failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
