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
engit publish --version dev --zip
```
