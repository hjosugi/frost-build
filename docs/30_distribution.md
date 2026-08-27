# Verified distribution

FrostBuild has one publication source: tagged GitHub Releases. Every install
path terminates at the same platform archive and `SHA256SUMS`; package-manager
metadata is generated from those final checksums rather than maintaining a
second set by hand.

## POSIX installer

Download and inspect the script when the environment requires that boundary,
then run it:

```sh
curl --proto '=https' --tlsv1.2 -fsSLo install.sh \
  https://raw.githubusercontent.com/hjosugi/frost-build/main/install.sh
less install.sh
sh install.sh
```

The default prefix is `$HOME/.local`. `--prefix DIR` changes it and
`--version X.Y.Z` pins an exact release. Without `--version`, the script reads
GitHub's latest stable release endpoint. Supported prebuilt POSIX hosts are
x86-64 Linux (static musl) and the macOS architectures actually present in the
release.

The prefix is not touched while downloading. The script fetches
`SHA256SUMS`, selects the exact platform asset by its whole filename, hashes the
archive, extracts it in temporary storage and requires `frost --version` to
match the requested tag. Only a complete candidate is staged under the target
prefix. `frostd`, man pages and completions are published first; `frost` is an
atomic rename performed last. A checksum mismatch therefore leaves neither a
new executable nor a half-created prefix.

Installed files use standard user-prefix locations:

```text
~/.local/bin/{frost,frostd}
~/.local/share/man/man1/frost*.1
~/.local/share/bash-completion/completions/frost
~/.local/share/zsh/site-functions/_frost
~/.local/share/fish/vendor_completions.d/frost.fish
```

## Package managers

Each release attaches `frostbuild.rb` and `frostbuild.json`. The Homebrew
formula carries per-architecture URLs and hashes and installs both binaries,
the complete manual and native completion locations. It can be installed as a
local formula or committed unchanged as `Formula/frostbuild.rb` in a tap:

```sh
curl -fsSLO https://github.com/hjosugi/frost-build/releases/latest/download/frostbuild.rb
brew install --formula ./frostbuild.rb
```

The Scoop manifest includes the x86-64 Windows archive hash, both executable
shims, GitHub `checkver` and an autoupdate rule backed by `SHA256SUMS`:

```powershell
scoop install https://github.com/hjosugi/frost-build/releases/latest/download/frostbuild.json
```

The release workflow renders both files only after all three archives exist,
runs `ruby -c` and JSON parsing, and publishes the exact generated files. A
mixed-version, incomplete, duplicate or malformed checksum set is rejected.

Winget and AUR publication remain maintainer-mediated because they write to
repositories with their own review and agreement boundaries. For Winget, feed
the Windows archive URL and version to `wingetcreate update`, validate the
generated manifests locally, and submit them through Microsoft's documented
review path. For AUR, update a `PKGBUILD` source URL to the Linux musl archive,
copy its value from `SHA256SUMS`, run `makepkg --verifysource` and
`makepkg --install`, then publish through the package maintainer account. No
release workflow accepts an agreement or writes either external repository.

## Manual and completions

`clap_mangen` walks the same `Cli::command()` tree used by `--help`, producing
`frost.1` and one page for every visible subcommand. The release-only generator
also emits bash, zsh, fish, PowerShell, Elvish and Nushell completion files from
that command tree. It is feature-gated so neither `clap_mangen` nor the
generator is linked into the normal binary. Release CI sets the date from the
stamped CHANGELOG, renders `man frost`, and copies the byte-identical common
tree into every platform archive.

## Explicit self-update

```sh
frost self-update --check
frost self-update
```

`--check` fetches only public release metadata and never writes. Updating then
selects the matching asset and `SHA256SUMS` URLs from that release, verifies the
archive before safe extraction, and runs the candidate's `--version` before the
cross-platform atomic replacement. A newer local development binary is never
downgraded. A binary found under Cargo's install root is refused and names
`cargo install --locked frostbuild-cli`; Cargo remains its owner.

There is no automatic invocation, background task, startup check, rollout
service or telemetry. The only network request is the command the user typed.

## Release and recurring gates

`.github/workflows/release.yml` generates common assets, proves the root manual
renders, builds three archives, derives checksums and package manifests, and
only then creates the tag and release. The normal CI performs the same asset
and package-manifest render as a dry run, including Ruby/JSON syntax checks.
`.github/workflows/distribution.yml`
runs daily on Linux and macOS against the real latest-release API, installs to
an isolated prefix, checks the version and companion files, then runs both
`self-update --check` and a checksum-verified atomic replacement before it
renders the installed manual. Local loopback tests cover the same layout and
the failure boundary without depending on GitHub.
