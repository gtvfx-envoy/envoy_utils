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

Set `ENVOY_BUNDLE_PUBLISH_ROOT` for bundles,
`ENVOY_STACK_PUBLISH_ROOT` for stacks, and `ENVOY_CONFIG_PUBLISH_ROOT` for the
redirect file, or pass `--output` explicitly. `ENVOY_CONFIG_PROD` is the
launcher's variable for reading the redirect file and is never used to publish.

## Stack publish symlink errors on network shares

`engit publish stack` no longer creates or updates `latest.estack`, so SMB
and other network-share publishes no longer fail on Windows symlink
permissions. When no `latest.estack` exists, Envoy resolves the newest
published stack version automatically by modified time.

## `publish redirect`: a version "was not found" but it is on the share

Each system's launcher looks for its own executable in a version's folder:
`bin/envoy.exe` on Windows, `bin/envoy` on Linux and macOS. `engit` checks for
the executable of the system it runs on, which is what the launcher on that
machine would do. A folder that was unpacked from the archive of another
system has the other executable, so the error names the one that is missing.

If the studio has several systems, name them, and unpack the archive of each
into the version's folder:

```console
engit publish redirect redirect.yaml --platform all
```

## `publish redirect`: "Another publish ... is still running"

Publishing into a folder is one person at a time, and a second publish waits
ten seconds for the first. Try again in a moment. The turn is an operating
system lock that is let go when a program ends however it ends, so a crashed
publish does not leave one behind. If the message keeps coming back, look for
an `engit` that is stuck on a slow share (for example in Task Manager).
`.redirect.lock` is an empty file that is meant to stay in the folder. Two
administrators who publish from different machines, or one from Windows and one
from WSL, may not see each other's turn on some locations (see [Two people
publishing](cli-reference/engit.md#two-people-publishing)); publish from one
place there.

## `publish redirect`: "Access is denied" when replacing the file

Another program has `redirect.yaml` open, such as a launcher that is reading
it, an editor, or a virus scanner. Over a network share even a launcher that
allows the file to be replaced makes the replace fail while it is reading.
`engit` tries again for about a second and a half, which covers a launcher's
read. A message that stays means the program keeps the file open (close the
editor) or that you may not change the file: check your write access to the
config root. The live file was not changed and the history copy of the new file
was removed. If the publish had first kept the file that was live (because the
history did not have it), that copy stays: that file did go live.

## `publish redirect`: "already exists ... try again"

Two publishes as the same user landed in the same second. History names are
never replaced, so the second one stopped before changing anything. Run it
again.

## `publish redirect`: "is not a folder" or "nothing has been published for team"

The config root must already exist, and a team's folder is made by that team's
first publish, so a mistyped `--output` or `--team` is an error instead of a new
folder that no launcher reads. `redirect show` and `redirect history` give the
same errors for a root or team that is not there.

## Legacy bundle artifact manifest

Rename `.envoy/bundle-artifacts.json` to `.envoy/publish-manifest.yaml`
and convert its JSON data to the strict YAML publish-manifest schema.
