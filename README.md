# Envoy Utils

Small command-line tools that operate on repositories and artifacts in the
[Envoy](https://github.com/gtvfx-contrib/gt-envoy) framework.

## Engit

`engit` provides semantic-version tagging, GitHub releases, changelog and
repository maintenance, Envoy bundle publishing, bundle checkout updates, and
named stack publishing.

See the [Engit CLI reference](docs/cli-reference/engit.md) for commands and
examples. Common failures are covered in the
[troubleshooting guide](docs/troubleshooting.md).

## Compatibility

| Envoy Utils | Envoy Core |
|-------------|------------|
| 0.1.x       | 0.5.1      |

The released `engit` executable statically links its Envoy Core dependency.
The compatibility entry identifies the Envoy configuration and bundle contract
against which it was built and tested.

## Development

Build and test the Rust workspace from the repository root:

```console
cd rust
cargo build
cargo test --workspace
```

Development launchers are available at `bin/engit` and `bin/engit.bat`.
