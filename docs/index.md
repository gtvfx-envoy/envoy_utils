# Envoy Utils

Envoy Utils provides repository, release, and publishing tools for the
[Envoy](https://gtvfx-envoy.github.io/envoy/) framework.

## Engit

`engit` supports semantic-version tagging, GitHub releases, changelog and
repository maintenance, bundle publishing, bundle checkout updates, and named
stack publishing.

- Read the [Engit CLI reference](cli-reference/engit.md) for commands and examples.
- Use the [troubleshooting guide](troubleshooting.md) for common failures.
- Browse the [generated Rust API reference][rust-api] for library details.

[rust-api]: https://gtvfx-envoy.github.io/envoy_utils/rustdoc/engit_core/

## Compatibility

The released `engit` executable statically links its Envoy Core dependency. The
exact pairing is recorded in each
[Envoy Utils release](https://github.com/gtvfx-envoy/envoy_utils/releases),
For the complete cross-repository release process, see the [Envoy maintainer release guide](https://gtvfx-envoy.github.io/envoy/maintainers/releasing/).

both in the release summary and in the attached `compatibility.json` file.
