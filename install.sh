#!/bin/sh
# Install a published FrostBuild release into a user-owned prefix.
#
# The archive is downloaded to a temporary directory, verified against the
# release's SHA256SUMS, unpacked and smoke-tested before the prefix is touched.
# Network, OS and architecture overrides exist so this exact script can be
# exercised against a local fixture without trusting the network in CI.

set -eu

program=${0##*/}
version=
prefix=${FROST_INSTALL_PREFIX:-${HOME:?HOME is not set}/.local}
api_url=${FROST_INSTALL_API_URL:-https://api.github.com/repos/hjosugi/frost-build/releases/latest}
release_base=${FROST_INSTALL_RELEASE_BASE_URL:-https://github.com/hjosugi/frost-build/releases/download}

say() {
    printf '%s: %s\n' "$program" "$1" >&2
}

die() {
    say "$1"
    exit 2
}

usage() {
    cat <<'EOF'
Install a checksum-verified FrostBuild release.

Usage: install.sh [--version X.Y.Z] [--prefix DIR]

Options:
  --version X.Y.Z  Install this exact release instead of the latest stable one
  --prefix DIR     Install under DIR (default: $HOME/.local)
  -h, --help       Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
    --version)
        [ "$#" -ge 2 ] || die "--version needs X.Y.Z"
        version=$2
        shift 2
        ;;
    --version=*) version=${1#*=}; shift ;;
    --prefix)
        [ "$#" -ge 2 ] || die "--prefix needs a directory"
        prefix=$2
        shift 2
        ;;
    --prefix=*) prefix=${1#*=}; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option '$1'" ;;
    esac
done

case "$prefix" in
'') die "the install prefix cannot be empty" ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl --fail --location --silent --show-error --output "$2" -- "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget --quiet --output-document "$2" -- "$1"; }
else
    die "neither curl nor wget is available"
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "neither sha256sum nor shasum is available; an unverified download will not be installed"
fi

temporary_parent=${TMPDIR:-/tmp}
staging=$(mktemp -d "${temporary_parent%/}/frost-install.XXXXXX") ||
    die "cannot create a temporary directory under $temporary_parent"
publish=
cleanup() {
    rm -rf "$staging"
    if [ -n "$publish" ]; then
        rm -rf "$publish"
    fi
}
trap cleanup EXIT HUP INT TERM

if [ -z "$version" ]; then
    fetch "$api_url" "$staging/latest.json" || die "cannot query the latest FrostBuild release"
    version=$(
        tr -d '\r\n' <"$staging/latest.json" |
            sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\([^"]*\)".*/\1/p'
    )
    [ -n "$version" ] || die "the latest release response names no vX.Y.Z tag"
fi

printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    die "'$version' is not an X.Y.Z version"

system=${FROST_INSTALL_OS:-$(uname -s)}
machine=${FROST_INSTALL_ARCH:-$(uname -m)}
case "$system/$machine" in
Linux/x86_64 | Linux/amd64) triple=x86_64-unknown-linux-musl ;;
Darwin/arm64 | Darwin/aarch64) triple=aarch64-apple-darwin ;;
Darwin/x86_64) triple=x86_64-apple-darwin ;;
*)
    die "no published FrostBuild release for $system $machine (supported: x86_64 Linux, arm64/x86_64 macOS)"
    ;;
esac

archive="frostbuild-v${version}-${triple}.tar.gz"
release_url="${release_base%/}/v${version}"
say "downloading frost $version ($triple)"
fetch "$release_url/SHA256SUMS" "$staging/SHA256SUMS" ||
    die "cannot download $release_url/SHA256SUMS"

expected=$(
    awk -v want="$archive" '
        { name = $2; sub(/^[*]/, "", name) }
        name == want { print $1; exit }
    ' "$staging/SHA256SUMS"
)
[ -n "$expected" ] || die "release $version publishes no checksum for $archive"
printf '%s' "$expected" | grep -Eq '^[0-9a-fA-F]{64}$' ||
    die "release $version publishes an invalid checksum for $archive"

fetch "$release_url/$archive" "$staging/$archive" ||
    die "cannot download $release_url/$archive"
actual=$(sha256_of "$staging/$archive")
if [ "$actual" != "$expected" ]; then
    say "checksum mismatch for $archive"
    say "  expected $expected"
    say "  got      $actual"
    die "the download was discarded and nothing was installed"
fi

tar -xzf "$staging/$archive" -C "$staging" || die "cannot unpack $archive"
unpacked="$staging/frostbuild-v${version}-${triple}"
[ -x "$unpacked/frost" ] || die "$archive contains no executable frost"
[ -x "$unpacked/frostd" ] || die "$archive contains no executable frostd"
reported=$("$unpacked/frost" --version 2>/dev/null || true)
[ "$reported" = "frost $version" ] ||
    die "$archive reports '$reported' instead of 'frost $version'"
[ -f "$unpacked/share/man/man1/frost.1" ] || die "$archive contains no frost.1"
for page in "$unpacked"/share/man/man1/*.1; do
    [ -f "$page" ] || die "$archive contains an invalid man page set"
done
[ -f "$unpacked/share/completions/frost.bash" ] || die "$archive contains no bash completion"
[ -f "$unpacked/share/completions/_frost" ] || die "$archive contains no zsh completion"
[ -f "$unpacked/share/completions/frost.fish" ] || die "$archive contains no fish completion"

# The verified tree is complete. Only now create the destination, then stage
# every installed file on the destination filesystem before atomic renames.
mkdir -p "$prefix"
publish="$prefix/.frost-install.$$"
(umask 077 && mkdir "$publish") || die "cannot stage files under $prefix"

mkdir -p "$publish/bin" "$publish/share/man/man1" \
    "$publish/share/bash-completion/completions" \
    "$publish/share/zsh/site-functions" \
    "$publish/share/fish/vendor_completions.d"
cp "$unpacked/frost" "$unpacked/frostd" "$publish/bin/"
chmod 755 "$publish/bin/frost" "$publish/bin/frostd"
for page in "$unpacked"/share/man/man1/*.1; do
    cp "$page" "$publish/share/man/man1/"
done
cp "$unpacked/share/completions/frost.bash" "$publish/share/bash-completion/completions/frost"
cp "$unpacked/share/completions/_frost" "$publish/share/zsh/site-functions/_frost"
cp "$unpacked/share/completions/frost.fish" "$publish/share/fish/vendor_completions.d/frost.fish"

mkdir -p "$prefix/bin" "$prefix/share/man/man1" \
    "$prefix/share/bash-completion/completions" \
    "$prefix/share/zsh/site-functions" \
    "$prefix/share/fish/vendor_completions.d"
mv -f "$publish/bin/frostd" "$prefix/bin/frostd"
for page in "$publish"/share/man/man1/*.1; do
    mv -f "$page" "$prefix/share/man/man1/${page##*/}"
done
mv -f "$publish/share/bash-completion/completions/frost" "$prefix/share/bash-completion/completions/frost"
mv -f "$publish/share/zsh/site-functions/_frost" "$prefix/share/zsh/site-functions/_frost"
mv -f "$publish/share/fish/vendor_completions.d/frost.fish" "$prefix/share/fish/vendor_completions.d/frost.fish"
# Publish the user-facing executable last: observing a new frost implies all
# of that release's companion files are already present.
mv -f "$publish/bin/frost" "$prefix/bin/frost"
rm -rf "$publish"

trap - EXIT HUP INT TERM
rm -rf "$staging"
say "installed frost $version in $prefix/bin"
case ":${PATH:-}:" in
*":$prefix/bin:"*) ;;
*) say "add $prefix/bin to PATH" ;;
esac
