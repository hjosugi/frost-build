# Verified distribution

FrostBuild has one publication source: tagged GitHub Releases. Every install
path terminates at the same platform archive and `SHA256SUMS`; package-manager
metadata is generated from those final checksums rather than maintaining a
second set by hand. From 0.14.0 every release also carries keyless Sigstore
signatures, GitHub SLSA build provenance and a CycloneDX SBOM per archive, so
a download can be traced to the workflow run that built it rather than only to
the checksum file published next to it.

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

The checksum proves the archive is the one `SHA256SUMS` lists; it cannot prove
who published that list, since anyone able to replace one release asset can
replace both. Two opt-in switches close that gap:

```sh
sh install.sh --verify-signature    # needs cosign
sh install.sh --verify-provenance   # needs an authenticated gh
# equivalently FROST_INSTALL_VERIFY_SIGNATURE=1 / FROST_INSTALL_VERIFY_PROVENANCE=1
```

`--verify-signature` checks `SHA256SUMS.sigstore.json` before the checksum list
is trusted; `--verify-provenance` checks the archive against
`frostbuild-vX.Y.Z.intoto.jsonl` after the checksum. Both are off by default
only because the script cannot assume either tool is installed, and a switch
that was asked for never degrades: a missing tool is refused before anything is
downloaded, and a failed check discards the download with the prefix
untouched. Releases before 0.14.0 have neither file and fail both switches.

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

## Verifying a release

Each release publishes, next to the archives and `SHA256SUMS`:

| Asset | What it is |
|---|---|
| `<archive>.sigstore.json` and `SHA256SUMS.sigstore.json` | cosign keyless signature bundles: signature, Fulcio certificate and Rekor transparency-log proof |
| `frostbuild-vX.Y.Z-<triple>.cdx.json` | CycloneDX 1.5 SBOM of that archive: `frostbuild-cli`'s resolved dependency graph for that target triple (it depends on `frostbuild-daemon`, so it covers `frostd` too) |
| `frostbuild-vX.Y.Z.intoto.jsonl` | GitHub's SLSA v1 build provenance for every archive, SBOM and `SHA256SUMS`, as a Sigstore bundle |

No key exists to lose or rotate. The signing certificate is issued by
Sigstore's Fulcio to the OIDC identity GitHub gives the release job, and it
names the workflow file and the ref it ran from. Verification therefore pins
that identity: `release.yml` of `hjosugi/frost-build`, run from `main`
(`workflow_dispatch`) or from a release tag. A signature made by any other
workflow — including the dry run below, or a fork — does not verify as a
release.

For one archive, by hand:

```sh
v=0.14.0
a=frostbuild-v$v-x86_64-unknown-linux-musl.tar.gz
gh release download v$v --repo hjosugi/frost-build \
  -p "$a" -p "$a.sigstore.json" -p SHA256SUMS -p SHA256SUMS.sigstore.json \
  -p "frostbuild-v$v.intoto.jsonl" -p "frostbuild-v$v-x86_64-unknown-linux-musl.cdx.json"

identity='^https://github\.com/hjosugi/frost-build/\.github/workflows/release\.yml@refs/(heads/main|tags/v[0-9]+\.[0-9]+\.[0-9]+)$'
issuer=https://token.actions.githubusercontent.com

cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp "$identity" --certificate-oidc-issuer "$issuer" SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
cosign verify-blob --bundle "$a.sigstore.json" \
  --certificate-identity-regexp "$identity" --certificate-oidc-issuer "$issuer" "$a"
gh attestation verify "$a" --bundle "frostbuild-v$v.intoto.jsonl" \
  --repo hjosugi/frost-build --signer-workflow hjosugi/frost-build/.github/workflows/release.yml
```

Without the downloaded bundle, `gh attestation verify "$a" --repo
hjosugi/frost-build` fetches the same attestation from GitHub's API. For every
asset of a release at once — the same commands, plus a check that each SBOM
describes that version — run `scripts/verify_release.sh DIR X.Y.Z` over the
downloaded release (`gh release download vX.Y.Z --dir DIR`).

What this proves, and what it does not: a verified archive was built by this
repository's release workflow from a commit on `main` or a release tag, and
has not changed since. It does not prove that commit is benign — anyone who can
push to `main` can run the release workflow — and the SBOM lists what was
compiled in, not whether any of it is vulnerable (`cargo-deny` in CI answers
that).

`frost self-update` and `frostw` still verify the SHA-256 alone: both run
unattended and cannot assume `cosign` or an authenticated `gh`.

## Reproducible archives

The question #148 asked is whether rebuilding the same tag gives the same
checksums. `.github/workflows/release-dry-run.yml` answers it on every run: it
builds each archive twice from one commit, on two separate runners, once in the
usual checkout and once under a longer checkout path, with the release
toolchain and commands, and compares the bytes. The first measurement was run
[36011660153](https://github.com/hjosugi/frost-build/actions/runs/36011660153)
on 24 September 2026:

| Archive | Two builds of one commit, at two checkout paths | What differs |
|---|---|---|
| `x86_64-unknown-linux-musl.tar.gz` | **identical**: `frost`, `frostd` and the archive | nothing |
| `aarch64-apple-darwin.tar.gz` | 48 bytes of each binary differ | the linker's 16-byte `LC_UUID`, and the 32-byte hash of the one code-signature page that contains it; every other byte is identical |
| `x86_64-pc-windows-msvc.zip` | about 300 bytes of each binary differ | `link.exe` stamps the link time into the PE header and debug directory and a fresh GUID into the PDB reference, because rustc does not pass `/Brepro`; the two runner images also carried different MSVC tool builds (36256 and 36257), which the Rich header records, and the PE header landed at a different offset (256 and 248) |

A second run,
[36012634795](https://github.com/hjosugi/frost-build/actions/runs/36012634795),
built the same sources again on fresh runners. At each checkout path the macOS
binaries were byte-identical to the first run's: the UUID follows the path, not
the machine or the time. `release.yml` always builds at the same checkout path,
so rebuilding a tag on a hosted macOS runner reproduces the macOS archive. The
Windows binaries differed again even at the same path. The dry run therefore
builds macOS and Windows twice at the release's path, on separate runners, and
once elsewhere.

The Linux result holds across checkout paths because Cargo compiles workspace
members from workspace-relative paths with path-independent crate hashes, a
release build carries no debug info or timestamps, and the one absolute path
left in the binary — registry sources in panic locations,
`/home/runner/.cargo/registry/src/...` — is the same on every hosted runner.
Rebuilding outside GitHub Actions therefore reproduces the checksum only with
the same toolchain (`rust-toolchain.toml`, the Ubuntu image's `musl-tools`) and
the same `CARGO_HOME` path; `--remap-path-prefix` would remove the last
condition and is not applied because nothing needs it yet.

Before this work the packing alone made every rebuild differ, whatever the
binaries: GNU tar and bsdtar recorded file times from the checkout and the
build, the runner's user and group, and directory-listing order; gzip recorded
when it ran; `Compress-Archive` recorded local times. `scripts/package_release.py`
now writes all three archives from explicit metadata — sorted entries, the
release commit's timestamp, uid/gid 0 with empty names, fixed modes, a gzip
header with neither name nor time, ZIP entries recorded as Unix entries on every
packing host — and a unit test packs twice from files with different times and
requires identical bytes.

Decisions:

- **Linux is gated.** The dry run fails if its two musl builds differ, so the
  claim above stays true as dependencies and runner images change.
- **macOS is gated at the release's path.** The dry run fails if two builds at
  the same checkout path differ; the different-path build is only reported,
  since its difference is confined to the UUID (and the signature hash that
  covers it). Suppressing the UUID with `-Wl,-no_uuid` to make paths irrelevant
  is not an option — crash reporting and symbolication key on it.
- **Windows is recorded, not gated.** `-C link-arg=/Brepro` would replace the
  timestamp with a content hash and derive the PDB GUID from the content; the
  Rich header still follows whichever MSVC toolset the hosted image carries,
  which this repository does not pin. Adopting `/Brepro` is a separate change,
  and this job is how it would be measured.

Every comparison, gated or not, is printed in the dry run's summary on every
run, so a toolchain or image change that fixes or worsens one is visible
without anyone having to look for it. None of this weakens the
verification above: signatures and provenance attest the bytes that were
published, whether or not a rebuild could recreate them.

## Release and recurring gates

`.github/workflows/release.yml` generates common assets, proves the root manual
renders, builds three archives, then — in its `integrity` job — writes
checksums and SBOMs, signs, attests, and verifies the result with
`scripts/verify_release.sh`, all before anything is published. Only then does
`publish` derive package manifests and create the tag and release, so a build,
signing or verification failure leaves no tag behind. The normal CI performs
the same asset and package-manifest render as a dry run, including Ruby/JSON
syntax checks.

`.github/workflows/release-dry-run.yml` runs when the release machinery,
`install.sh` or `Cargo.lock` changes, and weekly. It builds the Linux archive
as `release.yml` does, twice, and the macOS and Windows archives three times,
and compares them as described above. It signs and attests one Linux copy with
this workflow's own identity and verifies it with the published procedure. Then it
requires refusal, from `cosign`, `gh attestation`, `sha256sum`,
`scripts/verify_release.sh` and `install.sh --verify-signature` alike, of an
archive with one flipped byte and of an archive published with a matching,
rewritten `SHA256SUMS`; and it requires the published release command to
refuse the dry run's own genuine signatures.

`.github/workflows/distribution.yml`
runs daily on Linux and macOS against the real latest-release API, installs to
an isolated prefix, checks the version and companion files, then runs both
`self-update --check` and a checksum-verified atomic replacement before it
renders the installed manual. For a signed release it also downloads every
asset and runs `scripts/verify_release.sh` on it, and installs again with
`--verify-signature --verify-provenance`. Local loopback tests cover the same
layout and the failure boundary without depending on GitHub; stand-in `cosign`
and `gh` executables pin which file each check is given, with which identity,
and that a refusal stops the installation.
