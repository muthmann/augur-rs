#!/usr/bin/env bash
#
# Smoke-test a built AppImage.
#
# An AppImage that builds is not the same as an AppImage that works: a missing
# desktop entry or a mismatched icon name produces a file that runs but never
# appears in any application menu. Unpack it and check what a desktop
# environment would actually read.
#
# usage: verify-appimage.sh <dist-dir>

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

dist_dir="${1:-dist}"
dist_dir="$(cd "$dist_dir" && pwd)"
version="$(augur_version)"

appimage="$dist_dir/AugurRS-$version-linux-x86_64.AppImage"
tarball="$dist_dir/augur-$version-linux-x86_64.tar.gz"

for artifact in "$appimage" "$tarball"; do
  [ -f "$artifact" ] || { echo "missing artifact: $artifact" >&2; exit 1; }
done
[ -x "$appimage" ] || { echo "the AppImage is not executable" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

augur_log "unpacking $(basename "$appimage")"
# --appimage-extract rather than a mount: CI runners have no FUSE.
(cd "$work" && "$appimage" --appimage-extract >/dev/null)
root="$work/squashfs-root"

augur_log "checking the desktop entry"
desktop="$root/AugurRS.desktop"
[ -f "$desktop" ] || { echo "no desktop entry in the AppImage root" >&2; exit 1; }
grep -q '^Exec=AugurRS' "$desktop" || { echo "desktop entry has the wrong Exec" >&2; exit 1; }

# The icon is resolved by the desktop entry's Icon= key, matched by basename.
# Getting this wrong is how the AppImage ends up with no icon anywhere.
icon_name="$(sed -n 's/^Icon=//p' "$desktop" | head -1)"
[ -n "$icon_name" ] || { echo "desktop entry declares no icon" >&2; exit 1; }
[ -f "$root/$icon_name.png" ] || {
  echo "no $icon_name.png in the AppImage root to satisfy Icon=$icon_name" >&2
  exit 1
}

augur_log "checking the payload"
for binary in AugurRS augur; do
  [ -x "$root/usr/bin/$binary" ] || { echo "missing $binary" >&2; exit 1; }
done

augur_log "running the packaged CLI"
"$root/usr/bin/augur" --version

staging="$(mktemp -d)"
tar -xzf "$tarball" -C "$staging"
"$staging/augur-linux/bin/augur" --version
rm -rf "$staging"

augur_log "Linux artifacts verified"
