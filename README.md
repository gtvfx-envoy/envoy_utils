# Envoy Utils

Small command-line tools that operate on repositories and artifacts in the
[Envoy](https://github.com/gtvfx-envoy/envoy) framework.

## Engit

`engit` provides semantic-version tagging, GitHub releases, changelog and
repository maintenance, Envoy bundle publishing, bundle checkout updates,
named stack publishing, local cross-repo development links, and
release-train orchestration.

See the [Engit CLI reference](docs/cli-reference/engit.md) for commands and
examples. Common failures are covered in the
[troubleshooting guide](docs/troubleshooting.md).

The documentation site is built automatically with ProperDocs and published to
[GitHub Pages](https://gtvfx-envoy.github.io/envoy_utils/). Generated Rust
API documentation is available from the site navigation.

## Compatibility

The released `engit` executable statically links its Envoy Core dependency.
Each [Envoy Utils release](https://github.com/gtvfx-envoy/envoy_utils/releases)
identifies the exact Envoy Core tag and commit against which it was built and
tested. The release also includes `compatibility.json` for automated consumers.

## Development

Build and test the Rust workspace from the repository root:

```console
cd rust
cargo build
cargo test --workspace
```

Development launchers are available at `bin/engit` and `bin/engit.bat`.

Working on an unreleased Envoy change and want to test it here before either
side is tagged? `engit dev link rust ..\envoy` points this workspace's
`envoy-core` dependency at a local checkout (reversible with
`engit dev unlink rust`); `engit dev link python ..\envoy` does the same for
Envoy's Python API via an isolated dev bundle. If you have an Envoy Stack
active, add the printed bundle path directly to it -- Envoy resolves
bundles from the active Stack and does not consult `ENVOY_BNDL_ROOTS` while
one is set; otherwise add its parent directory to `ENVOY_BNDL_ROOTS` (or
reference it directly). See
[the CLI reference](docs/cli-reference/engit.md#engit-dev) for details.
