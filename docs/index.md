# Envoy Utils

Envoy Utils provides repository, release, and publishing tools for the
[Envoy](https://gtvfx-contrib.github.io/gt-envoy/) framework.

## Engit

`engit` supports semantic-version tagging, GitHub releases, changelog and
repository maintenance, bundle publishing, bundle checkout updates, and named
stack publishing.

- Read the [Engit CLI reference](cli-reference/engit.md) for commands and examples.
- Use the [troubleshooting guide](troubleshooting.md) for common failures.
- Browse the [generated Rust API reference][rust-api] for library details.

[rust-api]: https://gtvfx-contrib.github.io/gt-envoy_utils/rustdoc/engit_core/

## Compatibility

| Envoy Utils | Envoy Core |
|---|---|
| 0.1.x | 0.5.1 |

The released `engit` executable statically links its Envoy Core dependency. The
compatibility entry identifies the Envoy configuration and bundle contract
against which it was built and tested.
