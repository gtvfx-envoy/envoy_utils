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
same errors for a root or team that is not there. `publish shim` also needs the
root to exist, and makes `shim/` in it itself.

## `publish shim`: "is a pre-release, and publishing moves the pointer"

Installed launchers follow `shim/current.json` within a day, so moving it to a
pre-release is something to ask for. To try the release on the share first, put
it there without moving the pointer, and move the pointer later by publishing
the same release again:

```console
engit publish shim downloads/envoy-v0.8.1-rc.1 --no-pointer
engit publish shim downloads/envoy-v0.8.1-rc.1 --allow-prerelease
```

## `publish shim`: "does not match SHA256SUMS", "does not list", or "has no SHA256SUMS"

The archives are checked against the release's `SHA256SUMS`, and nothing is
written when they do not agree. Download the archive again, or the `SHA256SUMS`
from the same release: a `SHA256SUMS` from another release does not list these
archives. It is not downloaded for you, and it is needed even for archives you
built yourself (write one with `sha256sum` or `Get-FileHash`, one `<hash>
<name>` line for each archive).

`SHA256SUMS` is read as UTF-8, with or without a byte order mark, or as UTF-16
with one: Windows PowerShell 5.1 writes UTF-16 with `>` and `Out-File`, and that
is fine. "has a line that is not a SHA-256 sum" shows the start of the line: it
is a file of another kind, or UTF-16 with no byte order mark. Write it again as
UTF-8 (`Out-File -Encoding utf8`, or PowerShell 7).

## `publish shim`: "is not a launcher archive", "cannot be unpacked safely", "lists a name more than once", or "is not a plain file"

The archive is not what a release builds. A launcher archive holds the
launcher, its installers and a licence: a few files and a few megabytes, and
the limits for what is not one are far above that (they are listed under
[`engit publish shim`](cli-reference/engit.md#engit-publish-shim)). The message
names the file and what is wrong with it. Download it again from the release;
if it is one you built yourself, check what went into it: a stray folder of
build output, a link, or a ZIP file made by adding the same file twice are the
usual causes.

## `publish shim`: "is written for schema ..."

`shim/current.json` on the share was written by a newer `engit` with a layout
this one does not know, so it is not overwritten, and nothing is published into
that shim folder, not even with `--no-pointer`. Update `engit` and publish
again.

## `publish shim`: "is on the share already and is not the ..."

A published launcher version is never changed, because installed launchers copy
from it and an install can be half way through. The version on the share does
not have exactly the files of the archive you named: either it was published
from another build of the same version, or someone changed a file in it. Publish
the build you mean as a new version. If the one on the share is wrong and nobody
uses it, an administrator can remove its folder, and then publish again.

## `publish shim`: "does not run here" or "reports ..., not ..."

The launcher for the system `engit` runs on is run before anything is written,
and has to say it is the version being published. If the message says it does
not run, a virus scanner may have blocked the unpacked program: the message
gives the reason. The launcher is run from a file in the temporary folder, so
where programs may not run from there (a mount that does not allow it, a
policy), set `TMPDIR` (`TEMP` on Windows) to a folder where they may and
publish again. If it reports another version, or more than one line, the
archive is not the one its name says, so it was not published.

## `publish shim`: "None on the share for ..."

Only the platforms whose archive was in the folder (or published earlier with
the same version) have a launcher on the share. If a platform listed there has
machines, put its archive in the folder and publish again: the platforms that
are there already are left alone, and only the new one is written. When the
pointer moved, a note also names the platforms that had a launcher in the
version before and have none in this one.

## Legacy bundle artifact manifest

Rename `.envoy/bundle-artifacts.json` to `.envoy/publish-manifest.yaml`
and convert its JSON data to the strict YAML publish-manifest schema.
