# Engit Troubleshooting

## No existing tags

Provide an explicit semantic version when creating the first tag:

```console
engit tag --version 0.1.0
```

## GitHub CLI unavailable

`engit release` requires the [GitHub CLI](https://cli.github.com/) to be
installed and authenticated:

```console
gh auth login
```

## Publishing without a version tag

Use the development version for a local test archive:

```console
engit publish bundle --output dist --version dev --zip
```

## Missing canonical publish root

Set `ENVOY_BUNDLE_PUBLISH_ROOT` for bundles and
`ENVOY_STACK_PUBLISH_ROOT` for stacks, or pass `--output` explicitly.

## Stack publish symlink errors on network shares

`engit publish stack` no longer creates or updates `latest.estack`, so SMB
and other network-share publishes no longer fail on Windows symlink
permissions. When no `latest.estack` exists, Envoy resolves the newest
published stack version automatically by modified time.

## Legacy bundle artifact manifest

Rename `.envoy/bundle-artifacts.json` to `.envoy/publish-manifest.yaml`
and convert its JSON data to the strict YAML publish-manifest schema.
