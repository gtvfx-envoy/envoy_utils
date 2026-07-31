# Envoy Utils

Small command-line tools that operate on repositories and artifacts in the
[Envoy](https://github.com/gtvfx-envoy/envoy) framework.

## Engit

`engit` provides semantic-version tagging, GitHub releases, changelog and
repository maintenance, Envoy bundle publishing, bundle checkout updates, and
named stack publishing.

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
