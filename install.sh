#!/bin/sh
# rmon installer
#
#   curl -fsSL https://raw.githubusercontent.com/pgkt04/rmon/main/install.sh | sh
#
# env overrides:
#   RMON_VERSION   tag to install (default: latest release)
#   RMON_BIN_DIR   where to put the binary (default: /usr/local/bin, else ~/.local/bin)

set -eu

REPO="pgkt04/rmon"
VERSION="${RMON_VERSION:-}"
BIN_DIR="${RMON_BIN_DIR:-}"

die() {
	printf 'install: %s\n' "$1" >&2
	exit 1
}

info() { printf '%s\n' "$1" >&2; }

need() {
	command -v "$1" >/dev/null 2>&1 || die "need $1 on PATH"
}

# curl if we have it, else wget; everything below goes through these two
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
	# resolve the /releases/latest redirect instead of hitting the api, which
	# rate limits by ip and breaks on shared/ci networks
	latest_tag() {
		curl -fsSLI -o /dev/null -w '%{url_effective}' \
			"https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||'
	}
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
	latest_tag() {
		wget -qS -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 |
			sed -n 's|.*[Ll]ocation:.*/tag/||p' | tr -d ' \r' | tail -n 1
	}
else
	die "need curl or wget on PATH"
fi

need tar
need uname

# true if we can write $1, or create it -- a dir that does not exist yet is
# fine as long as its nearest existing ancestor is writable
can_write() {
	d="$1"
	while [ ! -e "$d" ]; do
		parent="${d%/*}"
		[ -n "$parent" ] || parent=/
		[ "$parent" != "$d" ] || break
		d="$parent"
	done
	[ -d "$d" ] && [ -w "$d" ]
}

# sha256sum on linux, shasum on macos; refuse to install unverified bytes
if command -v sha256sum >/dev/null 2>&1; then
	sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	die "need sha256sum or shasum on PATH"
fi

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
Linux) os=linux ;;
Darwin) os=macos ;;
*) die "unsupported os: $os (rmon runs on Linux and macOS)" ;;
esac

case "$arch" in
x86_64 | amd64) arch=x86_64 ;;
aarch64 | arm64) arch=aarch64 ;;
*) die "unsupported architecture: $arch" ;;
esac

# apple names the same chip arm64, the release assets follow suit
[ "$os" = macos ] && [ "$arch" = aarch64 ] && arch=arm64

triple="$os-$arch"

if [ -z "$VERSION" ]; then
	info "resolving latest release..."
	VERSION="$(latest_tag)"
	[ -n "$VERSION" ] || die "could not resolve the latest release tag; set RMON_VERSION"
fi
# accept both 1.2.3 and v1.2.3
case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac

# pick an install dir: an explicit one, else /usr/local/bin (with sudo if we
# need it), else a per-user fallback
sudo=""
if [ -z "$BIN_DIR" ]; then
	if can_write /usr/local/bin; then
		BIN_DIR=/usr/local/bin
	elif command -v sudo >/dev/null 2>&1; then
		BIN_DIR=/usr/local/bin
		sudo=sudo
	else
		BIN_DIR="$HOME/.local/bin"
	fi
elif ! can_write "$BIN_DIR"; then
	if command -v sudo >/dev/null 2>&1; then
		sudo=sudo
	else
		die "$BIN_DIR is not writable and sudo is not available"
	fi
fi

tarball="rmon-$VERSION-$triple.tar.gz"
base="https://github.com/$REPO/releases/download/$VERSION"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

info "downloading $tarball"
fetch "$base/$tarball" "$tmp/$tarball" ||
	die "no build for $triple at $VERSION -- check https://github.com/$REPO/releases"

info "verifying checksum"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" ||
	die "could not download SHA256SUMS for $VERSION"

want="$(sed -n "s|^\([0-9a-f]\{64\}\)[ *]*$tarball\$|\1|p" "$tmp/SHA256SUMS")"
[ -n "$want" ] || die "$tarball is not listed in SHA256SUMS"
got="$(sha256 "$tmp/$tarball")"
[ "$want" = "$got" ] || die "checksum mismatch: expected $want, got $got"

tar -xzf "$tmp/$tarball" -C "$tmp"
[ -f "$tmp/rmon" ] || die "tarball did not contain an rmon binary"
chmod 755 "$tmp/rmon"

info "installing to $BIN_DIR"
$sudo mkdir -p "$BIN_DIR" || die "could not create $BIN_DIR"
# copy next to the target then rename: rename is atomic and dodges the
# "Text file busy" you get from writing over an rmon that is still running
staged="$BIN_DIR/.rmon.new.$$"
$sudo cp "$tmp/rmon" "$staged" || die "could not write to $BIN_DIR"
$sudo chmod 755 "$staged"
$sudo mv -f "$staged" "$BIN_DIR/rmon" || {
	$sudo rm -f "$staged"
	die "could not replace $BIN_DIR/rmon"
}

# never fail the install just because we could not run the thing
version="$("$BIN_DIR/rmon" --version 2>/dev/null)" || version="rmon $VERSION"
info "installed $version to $BIN_DIR/rmon"

case ":$PATH:" in
*":$BIN_DIR:"*) info "run: rmon" ;;
*)
	info ""
	info "$BIN_DIR is not on your PATH. add this to your shell profile:"
	info "    export PATH=\"$BIN_DIR:\$PATH\""
	;;
esac
