"""Release and Envoy Core compatibility automation for Envoy Utils."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


SEMVER_PATTERN = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
WORKSPACE_PACKAGE_NAMES = {"engit-cli", "engit-core"}
DIRECT_ENVOY_APIS = (
    "envoy_core::discovery::{discover_bundles_auto, Bundle}",
    "envoy_core::stack_registry::{publish_stack, STACK_ROOTS_VAR}",
)


def validateVersion(version: str) -> str:
    """Validate and return an unprefixed semantic version."""
    if not SEMVER_PATTERN.fullmatch(version):
        raise ValueError(f"Invalid semantic version: {version!r}")
    return version


def validateTag(tag: str) -> str:
    """Validate and return a v-prefixed semantic-version tag."""
    if not tag.startswith("v"):
        raise ValueError(f"Release tag must start with 'v': {tag!r}")
    validateVersion(tag[1:])
    return tag


def replaceOne(
    path: Path, pattern: re.Pattern, replacement: str, description: str
) -> None:
    """Apply one deterministic text replacement."""
    contents = path.read_text(encoding="utf-8")
    updated, replacement_count = pattern.subn(replacement, contents, count=1)
    if replacement_count != 1:
        raise RuntimeError(
            f"Expected one {description} in {path}; found {replacement_count}."
        )
    path.write_text(updated, encoding="utf-8")


def replaceWorkspaceVersion(manifest_path: Path, version: str) -> None:
    """Replace the workspace package version."""
    replaceOne(
        manifest_path,
        re.compile(r"(?ms)(^\[workspace\.package\]\s*.*?)^version\s*=\s*\"[^\"]+\"$"),
        rf'\g<1>version = "{version}"',
        "[workspace.package] version",
    )


def replaceEnvoyDependency(manifest_path: Path, envoy_tag: str) -> None:
    """Replace the Envoy Core Git tag and exact crate version together."""
    envoy_version = validateTag(envoy_tag)[1:]
    replacement = (
        'envoy-core = { git = "https://github.com/gtvfx-contrib/gt-envoy", '
        f'tag = "{envoy_tag}", version = "={envoy_version}" }}'
    )
    replaceOne(
        manifest_path,
        re.compile(r"(?m)^envoy-core\s*=\s*\{[^\n]+\}$"),
        replacement,
        "envoy-core workspace dependency",
    )


def parseManifest(repository_root: Path) -> tuple[str, str, str]:
    """Return the Utils version and Envoy tag/version from Cargo.toml."""
    contents = (repository_root / "rust" / "Cargo.toml").read_text(encoding="utf-8")
    version_match = re.search(
        r"(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*\"([^\"]+)\"$",
        contents,
    )
    dependency_match = re.search(
        r'(?m)^envoy-core\s*=\s*\{[^\n]*tag = "([^"]+)"[^\n]*version = "=([^"]+)"[^\n]*\}$',
        contents,
    )
    if version_match is None or dependency_match is None:
        raise RuntimeError(
            "Cargo.toml has an invalid release or envoy-core dependency shape."
        )
    return version_match.group(1), dependency_match.group(1), dependency_match.group(2)


def parseLockfile(repository_root: Path) -> tuple[dict[str, str], str, str, str]:
    """Return local versions and the locked Envoy version/tag/commit."""
    contents = (repository_root / "rust" / "Cargo.lock").read_text(encoding="utf-8")
    package_versions = {}
    envoy_version = ""
    envoy_tag = ""
    envoy_commit = ""
    for package_block in re.split(r"(?m)^\[\[package\]\]\s*$", contents)[1:]:
        name_match = re.search(r'(?m)^name = "([^"]+)"$', package_block)
        version_match = re.search(r'(?m)^version = "([^"]+)"$', package_block)
        if name_match is None or version_match is None:
            continue
        package_name = name_match.group(1)
        if package_name in WORKSPACE_PACKAGE_NAMES:
            package_versions[package_name] = version_match.group(1)
        if package_name == "envoy-core":
            source_match = re.search(
                r'(?m)^source = "git\+https://github\.com/gtvfx-contrib/gt-envoy\?tag=([^#"]+)#([0-9a-f]+)"$',
                package_block,
            )
            if source_match:
                envoy_version = version_match.group(1)
                envoy_tag = source_match.group(1)
                envoy_commit = source_match.group(2)
    if not envoy_version or not envoy_tag or not envoy_commit:
        raise RuntimeError("Cargo.lock has no resolved Envoy Core Git dependency.")
    return package_versions, envoy_version, envoy_tag, envoy_commit


def checkRelease(
    repository_root: Path,
    expected_version: str | None = None,
    expected_envoy_tag: str | None = None,
) -> dict:
    """Validate release versions, the Envoy pin, and the lockfile."""
    utils_version, envoy_tag, envoy_version = parseManifest(repository_root)
    package_versions, locked_version, locked_tag, locked_commit = parseLockfile(
        repository_root
    )
    validateVersion(utils_version)
    validateTag(envoy_tag)
    if envoy_version != envoy_tag[1:]:
        raise RuntimeError(
            f"Envoy tag {envoy_tag} disagrees with exact version {envoy_version}."
        )
    if locked_version != envoy_version or locked_tag != envoy_tag:
        raise RuntimeError(
            f"Cargo.lock Envoy {locked_tag}/{locked_version} disagrees with "
            f"Cargo.toml {envoy_tag}/{envoy_version}."
        )
    if set(package_versions) != WORKSPACE_PACKAGE_NAMES:
        raise RuntimeError("Cargo.lock is missing an Envoy Utils workspace package.")
    if any(version != utils_version for version in package_versions.values()):
        raise RuntimeError(
            f"Envoy Utils workspace {utils_version} disagrees with Cargo.lock {package_versions}."
        )
    if expected_version and utils_version != validateVersion(expected_version):
        raise RuntimeError(
            f"Expected Envoy Utils {expected_version}, found {utils_version}."
        )
    if expected_envoy_tag and envoy_tag != validateTag(expected_envoy_tag):
        raise RuntimeError(
            f"Expected Envoy Core {expected_envoy_tag}, found {envoy_tag}."
        )
    return {
        "utils_version": utils_version,
        "envoy_version": envoy_version,
        "envoy_tag": envoy_tag,
        "envoy_commit": locked_commit,
    }


def prepareRelease(repository_root: Path, version: str, envoy_tag: str) -> None:
    """Update release versions and resolve the new Envoy Core tag."""
    validated_version = validateVersion(version)
    validated_tag = validateTag(envoy_tag)
    rust_root = repository_root / "rust"
    replaceWorkspaceVersion(rust_root / "Cargo.toml", validated_version)
    replaceEnvoyDependency(rust_root / "Cargo.toml", validated_tag)
    subprocess.run(["cargo", "update", "-p", "envoy-core"], cwd=rust_root, check=True)
    subprocess.run(["cargo", "check", "--workspace"], cwd=rust_root, check=True)
    checkRelease(repository_root, validated_version, validated_tag)


def replaceWithLocalEnvoy(manifest_path: Path, envoy_root: Path) -> None:
    """Replace the pinned Envoy dependency with a candidate local crate."""
    replacement = (
        f'envoy-core = {{ path = "{(envoy_root / "rust" / "envoy-core").as_posix()}" }}'
    )
    replaceOne(
        manifest_path,
        re.compile(r"(?m)^envoy-core\s*=\s*\{[^\n]+\}$"),
        replacement,
        "envoy-core workspace dependency",
    )


def testCompatibility(
    repository_root: Path, envoy_root: Path, output_path: Path
) -> dict:
    """Test this source tree against a candidate Envoy Core in isolation."""
    with tempfile.TemporaryDirectory(
        prefix="envoy-utils-compat-"
    ) as temporary_directory:
        temporary_root = Path(temporary_directory) / "envoy_utils"
        shutil.copytree(
            repository_root,
            temporary_root,
            ignore=shutil.ignore_patterns(".git", "target", ".codebase-memory", "site"),
        )
        replaceWithLocalEnvoy(temporary_root / "rust" / "Cargo.toml", envoy_root)
        completed_process = subprocess.run(
            ["cargo", "test", "--workspace"],
            cwd=temporary_root / "rust",
            check=False,
        )
    result = {
        "classification": "review" if completed_process.returncode == 0 else "required",
        "return_code": completed_process.returncode,
        "direct_apis": list(DIRECT_ENVOY_APIS),
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


def gitOutput(repository_root: Path, arguments: list[str]) -> str:
    """Run Git and return stripped standard output."""
    completed_process = subprocess.run(
        ["git", *arguments],
        cwd=repository_root,
        check=True,
        capture_output=True,
        text=True,
    )
    return completed_process.stdout.strip()


def lockfileHasDependencyChanges(
    envoy_root: Path, base_tag: str, head_tag: str
) -> bool:
    """Return whether Envoy's lockfile changed beyond workspace version lines."""
    patch = gitOutput(
        envoy_root,
        ["diff", "--unified=0", base_tag, head_tag, "--", "rust/Cargo.lock"],
    )
    changed_lines = [
        line[1:].strip()
        for line in patch.splitlines()
        if line.startswith(("+", "-")) and not line.startswith(("+++", "---"))
    ]
    return any(not re.fullmatch(r'version = "[^"]+"', line) for line in changed_lines)


def workspaceManifestHasDependencyChanges(
    envoy_root: Path, base_tag: str, head_tag: str
) -> bool:
    """Return whether Envoy's workspace manifest changed beyond its release version."""
    patch = gitOutput(
        envoy_root,
        ["diff", "--unified=0", base_tag, head_tag, "--", "rust/Cargo.toml"],
    )
    changed_lines = [
        line[1:].strip()
        for line in patch.splitlines()
        if line.startswith(("+", "-")) and not line.startswith(("+++", "---"))
    ]
    return any(not re.fullmatch(r'version = "[^"]+"', line) for line in changed_lines)


def classifyImpact(envoy_root: Path, base_tag: str, head_tag: str) -> dict:
    """Classify Envoy changes that can affect Envoy Utils."""
    validateTag(base_tag)
    validateTag(head_tag)
    changed_files = tuple(
        line
        for line in gitOutput(
            envoy_root, ["diff", "--name-only", base_tag, head_tag]
        ).splitlines()
        if line
    )
    relevant_files = [
        file_path
        for file_path in changed_files
        if file_path.startswith("rust/envoy-core/src/")
        or file_path == "rust/envoy-core/Cargo.toml"
    ]
    if "rust/Cargo.lock" in changed_files and lockfileHasDependencyChanges(
        envoy_root, base_tag, head_tag
    ):
        relevant_files.append("rust/Cargo.lock")
    if "rust/Cargo.toml" in changed_files and workspaceManifestHasDependencyChanges(
        envoy_root, base_tag, head_tag
    ):
        relevant_files.append("rust/Cargo.toml")
    return {
        "classification": "review" if relevant_files else "none",
        "relevant": bool(relevant_files),
        "base_tag": base_tag,
        "head_tag": head_tag,
        "changed_files": list(changed_files),
        "relevant_files": relevant_files,
        "direct_apis": list(DIRECT_ENVOY_APIS),
    }


def writeCompatibilityMetadata(
    repository_root: Path, release_tag: str, output_root: Path
) -> None:
    """Write machine- and human-readable compatibility release metadata."""
    validated_tag = validateTag(release_tag)
    release_state = checkRelease(repository_root, validated_tag[1:])
    utils_commit = gitOutput(repository_root, ["rev-list", "-n", "1", validated_tag])
    metadata = {
        "schema_version": 1,
        "envoy_utils": {
            "version": release_state["utils_version"],
            "tag": validated_tag,
            "commit": utils_commit,
        },
        "envoy_core": {
            "version": release_state["envoy_version"],
            "tag": release_state["envoy_tag"],
            "commit": release_state["envoy_commit"],
        },
    }
    output_root.mkdir(parents=True, exist_ok=True)
    (output_root / "compatibility.json").write_text(
        json.dumps(metadata, indent=2) + "\n",
        encoding="utf-8",
    )
    release_notes = f"""Envoy Utils provides independently versioned Envoy developer tools.
These artifacts are checksummed but currently unsigned.

## Compatibility

| Envoy Utils | Envoy Core |
|---|---|
| `{validated_tag}` | `{release_state["envoy_tag"]}` |

This release statically links Envoy Core commit
`{release_state["envoy_commit"]}`. The same pairing is available in the
attached `compatibility.json` for automated consumers.
"""
    (output_root / "release-notes.md").write_text(release_notes, encoding="utf-8")


def writeGitHubOutput(output_path: Path, values: dict[str, str]) -> None:
    """Append simple values to a GitHub Actions output file."""
    with output_path.open("a", encoding="utf-8") as output_file:
        for name, value in values.items():
            output_file.write(f"{name}={value}\n")


def buildIssueReport(
    impact_path: Path,
    results_root: Path,
    run_url: str,
    output_path: Path,
    github_output: Path | None,
) -> None:
    """Build the downstream compatibility issue body and classification."""
    impact = json.loads(impact_path.read_text(encoding="utf-8"))
    results = [
        json.loads(result_path.read_text(encoding="utf-8"))
        for result_path in results_root.rglob("compatibility*.json")
    ]
    classification = (
        "required"
        if any(result.get("return_code") != 0 for result in results)
        else "review"
    )
    marker = f"<!-- envoy-compatibility:{impact['head_tag']} -->"
    relevant_files = "\n".join(
        f"- `{file_path}`" for file_path in impact["relevant_files"]
    )
    platforms = "\n".join(
        f"- Result {index}: `{result['classification']}` (exit {result['return_code']})"
        for index, result in enumerate(results, start=1)
    )
    body = f"""{marker}
Envoy {impact["head_tag"]} was released after the currently pinned {impact["base_tag"]}.

## Automated assessment

- Release impact: **{classification}**
- Validation run: {run_url}

### Relevant Envoy changes

{relevant_files}

### Linux and Windows compatibility

{platforms}

### Envoy Core APIs used directly

{chr(10).join(f"- `{api}`" for api in DIRECT_ENVOY_APIS)}

## Maintainer checklist

- [ ] Review behavioral changes, even if compilation and tests pass.
- [ ] Decide whether users need an Envoy Utils release linked to this Envoy Core.
- [ ] If releasing, use **Prepare Release** with Envoy tag `{impact["head_tag"]}`.
- [ ] If no release is needed, close this issue with the rationale.
"""
    output_path.write_text(body, encoding="utf-8")
    if github_output:
        writeGitHubOutput(
            github_output, {"classification": classification, "marker": marker}
        )


def buildParser() -> argparse.ArgumentParser:
    """Build the command-line parser."""
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    check_parser = subparsers.add_parser("check")
    check_parser.add_argument("--expect-version")
    check_parser.add_argument("--expect-envoy-tag")
    check_parser.add_argument("--github-output")
    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--version", required=True)
    prepare_parser.add_argument("--envoy-tag", required=True)
    compatibility_parser = subparsers.add_parser("compatibility")
    compatibility_parser.add_argument("--envoy-root", required=True)
    compatibility_parser.add_argument("--output", required=True)
    impact_parser = subparsers.add_parser("impact")
    impact_parser.add_argument("--envoy-root", required=True)
    impact_parser.add_argument("--base-tag", required=True)
    impact_parser.add_argument("--head-tag", required=True)
    impact_parser.add_argument("--output", required=True)
    impact_parser.add_argument("--github-output")
    metadata_parser = subparsers.add_parser("metadata")
    metadata_parser.add_argument("--release-tag", required=True)
    metadata_parser.add_argument("--output-dir", required=True)
    metadata_parser.add_argument("--repository-root")
    report_parser = subparsers.add_parser("report")
    report_parser.add_argument("--impact", required=True)
    report_parser.add_argument("--results", required=True)
    report_parser.add_argument("--run-url", required=True)
    report_parser.add_argument("--output", required=True)
    report_parser.add_argument("--github-output")
    return parser


def main(arguments: list[str] | None = None) -> int:
    """Run Envoy Utils release automation."""
    parser = buildParser()
    args = parser.parse_args(arguments)
    repository_root = Path(__file__).resolve().parent.parent
    try:
        if args.command == "check":
            state = checkRelease(
                repository_root, args.expect_version, args.expect_envoy_tag
            )
            print(
                f"Envoy Utils v{state['utils_version']} is pinned to {state['envoy_tag']} "
                f"at {state['envoy_commit']}."
            )
            if args.github_output:
                writeGitHubOutput(
                    Path(args.github_output),
                    {
                        "utils_version": state["utils_version"],
                        "envoy_tag": state["envoy_tag"],
                        "envoy_version": state["envoy_version"],
                        "envoy_commit": state["envoy_commit"],
                    },
                )
        elif args.command == "prepare":
            prepareRelease(repository_root, args.version, args.envoy_tag)
        elif args.command == "compatibility":
            result = testCompatibility(
                repository_root, Path(args.envoy_root), Path(args.output)
            )
            return int(result["return_code"])
        elif args.command == "impact":
            result = classifyImpact(Path(args.envoy_root), args.base_tag, args.head_tag)
            Path(args.output).write_text(
                json.dumps(result, indent=2) + "\n", encoding="utf-8"
            )
            if args.github_output:
                writeGitHubOutput(
                    Path(args.github_output),
                    {"relevant": str(result["relevant"]).lower()},
                )
        elif args.command == "metadata":
            writeCompatibilityMetadata(
                Path(args.repository_root).resolve()
                if args.repository_root
                else repository_root,
                args.release_tag,
                Path(args.output_dir),
            )
        else:
            buildIssueReport(
                Path(args.impact),
                Path(args.results),
                args.run_url,
                Path(args.output),
                Path(args.github_output) if args.github_output else None,
            )
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
