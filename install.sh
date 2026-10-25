#!/bin/sh
# rustune installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/dzulfikar08/rustune/master/install.sh | sh
#
# Downloads the release binary for this OS and CPU, checks its SHA256 and
# installs it as `rustune`. Settings (environment variables):
#   RUSTUNE_INSTALL_DIR  where to install (default: ~/.local/bin)
#   RUSTUNE_VERSION      release tag to install, e.g. v0.3.6 (default: latest)
#   RUSTUNE_BASE_URL     folder holding the release files (mirrors, testing)

set -eu

REPO=dzulfikar08/rustune

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# download URL FILE
download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

# sha256 FILE: prints the lowercase hex digest
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 -r "$1" | cut -d ' ' -f 1
    else
        die "sha256sum, shasum or openssl is required to verify the download"
    fi
}

os=$(uname -s)
case $os in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
        die "on Windows use Scoop (scoop bucket add rustune https://github.com/dzulfikar08/scoop-bucket; scoop install rustune) or download rustune-x86_64-windows.exe from https://github.com/$REPO/releases"
        ;;
    *) die "no prebuilt rustune for $os; build it with: cargo install --git https://github.com/$REPO" ;;
esac

arch=$(uname -m)
case $arch in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "no prebuilt rustune for $arch; build it with: cargo install --git https://github.com/$REPO" ;;
esac
# A shell running under Rosetta reports x86_64 on Apple silicon.
if [ "$os" = macos ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
    arch=aarch64
fi
if [ "$os" = linux ] && ldd --version 2>&1 | grep -qi musl; then
    die "the Linux build needs glibc; on musl systems build it with: cargo install --git https://github.com/$REPO"
fi

asset="rustune-$arch-$os"
if [ -n "${RUSTUNE_BASE_URL:-}" ]; then
    base=${RUSTUNE_BASE_URL%/}
elif [ -n "${RUSTUNE_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$RUSTUNE_VERSION"
else
    base="https://github.com/$REPO/releases/latest/download"
fi
dir=${RUSTUNE_INSTALL_DIR:-$HOME/.local/bin}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t rustune)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT TERM

say "Downloading $base/$asset"
download "$base/$asset" "$tmp/rustune" || die "download failed: $base/$asset"

expected=""
if download "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
    expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1; exit }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || die "$asset is not listed in $base/SHA256SUMS"
elif [ -z "${RUSTUNE_BASE_URL:-}" ]; then
    # Releases up to v0.3.6 have no SHA256SUMS: ask the GitHub API, which
    # records a sha256 digest for every release file.
    if [ -n "${RUSTUNE_VERSION:-}" ]; then
        api="https://api.github.com/repos/$REPO/releases/tags/$RUSTUNE_VERSION"
    else
        api="https://api.github.com/repos/$REPO/releases/latest"
    fi
    download "$api" "$tmp/release.json" || die "cannot fetch $api to verify the download"
    expected=$(tr ',' '\n' <"$tmp/release.json" | awk -v f="\"$asset\"" '
        /"name" *:/ { n = $0; sub(/.*"name" *: */, "", n); sub(/[ }\]]*$/, "", n); hit = (n == f) }
        hit && /"digest" *: *"sha256:/ { d = $0; sub(/.*"sha256:/, "", d); sub(/".*/, "", d); print d; exit }')
fi
[ -n "$expected" ] || die "no SHA256 checksum published for $asset; refusing to install"

actual=$(sha256 "$tmp/rustune")
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
if [ "$actual" != "$expected" ]; then
    die "checksum mismatch for $asset (expected $expected, got $actual)"
fi
say "Checksum OK ($actual)"

mkdir -p "$dir"
chmod 755 "$tmp/rustune"
mv -f "$tmp/rustune" "$dir/rustune"
say "Installed $dir/rustune"

if version=$("$dir/rustune" --version 2>/dev/null); then
    say "$version is ready."
elif [ "$os" = linux ]; then
    warn "rustune could not start. It needs the ALSA library (libasound.so.2):"
    warn "  Debian/Ubuntu: sudo apt install libasound2 (libasound2t64 on Ubuntu 24.04 and later)"
    warn "  Fedora: sudo dnf install alsa-lib    Arch: sudo pacman -S alsa-lib"
else
    warn "rustune was installed but could not start"
fi

case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        warn "$dir is not on your PATH. Add it, e.g. in ~/.profile or your shell's rc file:"
        warn "  export PATH=\"$dir:\$PATH\""
        ;;
esac

say "Optional: install yt-dlp (online search and streaming) and ffmpeg (exotic formats, HLS)."
say "Run 'rustune doctor' to check audio output and optional tools."
