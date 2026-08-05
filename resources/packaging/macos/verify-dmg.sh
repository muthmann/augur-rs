#!/usr/bin/env bash
#
# Smoke-test a built macOS disk image.
#
# Checks the properties that silently regressed in the past and that no
# compiler error would catch: that the binary really is universal (a
# single-arch build looks perfectly healthy on the machine that produced it,
# and simply fails to launch on Intel), and that the bundle's signature still
# validates after packaging.
#
# usage: verify-dmg.sh <dist-dir>

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

dist_dir="${1:-dist}"
dist_dir="$(cd "$dist_dir" && pwd)"
version="$(augur_version)"

dmg="$dist_dir/AugurRS-$version-macos-universal.dmg"
tarball="$dist_dir/augur-$version-macos-universal.tar.gz"

for artifact in "$dmg" "$tarball"; do
  [ -f "$artifact" ] || { echo "missing artifact: $artifact" >&2; exit 1; }
done

mount="$(mktemp -d "${TMPDIR:-/tmp}/augur-verify.XXXXXX")"
cleanup() {
  hdiutil detach "$mount" -quiet >/dev/null 2>&1 || true
  rm -rf "$mount"
}
trap cleanup EXIT

augur_log "mounting $(basename "$dmg")"
hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg" >/dev/null

app="$mount/AugurRS.app"
[ -d "$app" ] || { echo "disk image contains no AugurRS.app" >&2; exit 1; }
[ -L "$mount/Applications" ] || { echo "disk image has no Applications symlink" >&2; exit 1; }

augur_log "checking architectures"
archs="$(lipo -archs "$app/Contents/MacOS/AugurRS")"
echo "  $archs"
for arch in arm64 x86_64; do
  case " $archs " in
    *" $arch "*) ;;
    *) echo "the bundled binary is missing $arch" >&2; exit 1 ;;
  esac
done

augur_log "verifying the code signature"
codesign --verify --deep --strict "$app"

augur_log "checking bundle metadata"
plist="$app/Contents/Info.plist"
bundle_version="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist")"
[ "$bundle_version" = "$version" ] || {
  echo "Info.plist says $bundle_version, expected $version" >&2
  exit 1
}
[ -f "$app/Contents/Resources/AugurRS.icns" ] || {
  echo "the bundle has no icon" >&2
  exit 1
}

augur_log "running the packaged CLI"
staging="$(mktemp -d "${TMPDIR:-/tmp}/augur-cli.XXXXXX")"
tar -xzf "$tarball" -C "$staging"
"$staging/augur-macos/bin/augur" --version
rm -rf "$staging"

augur_log "macOS artifacts verified"
