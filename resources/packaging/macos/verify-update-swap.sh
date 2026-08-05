#!/usr/bin/env bash
#
# Verify the macOS in-app update mechanism against a real disk image.
#
# This mirrors, step for step, what `augur-update`'s `swap_macos_bundle` and
# `apply_macos` do (see augur-update/src/install.rs) — minus the final
# relaunch, which would open a window.
#
# It exists because this is the one update path that can quietly ruin an
# install: copy a bundle with the wrong tool and the signature's extended
# attributes are lost, swap it in the wrong order and a failure leaves a
# half-written bundle, and in either case the app stops launching with an
# error that says nothing useful. A unit test cannot catch any of that — only
# doing it to a real bundle can.
#
# usage: verify-update-swap.sh <dist-dir>

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

dist_dir="${1:-dist}"
dist_dir="$(cd "$dist_dir" && pwd)"
version="$(augur_version)"
dmg="$dist_dir/AugurRS-$version-macos-universal.dmg"

[ -f "$dmg" ] || { echo "missing disk image: $dmg" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/augur-swap.XXXXXX")"
mount="$(mktemp -d "${TMPDIR:-/tmp}/augur-swap-mnt.XXXXXX")"
cleanup() {
  hdiutil detach "$mount" -quiet >/dev/null 2>&1 || true
  rm -rf "$mount" "$work"
}
trap cleanup EXIT

augur_log "staging a pretend existing installation"
hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg" >/dev/null
installed="$work/AugurRS.app"
ditto "$mount/AugurRS.app" "$installed"

# Mark the "old" install so the swap can be proven to have replaced it rather
# than silently leaving the original in place.
marker="$installed/Contents/Resources/.pre-update-marker"
touch "$marker"

augur_log "applying the update the way augur-update does"
staged="$installed.update-staged"
retired="$installed.update-old"
rm -rf "$staged" "$retired"

# 1. ditto, not cp: it is the only copy that preserves the extended attributes
#    the code signature is sealed over.
ditto "$mount/AugurRS.app" "$staged"
# 2. move the old bundle aside *before* moving the new one in, so an
#    interruption leaves a complete bundle at one path or the other.
mv "$installed" "$retired"
mv "$staged" "$installed"
rm -rf "$retired"
# 3. re-sign: the swap changes the path the bundle was sealed at.
codesign --force --deep --sign - "$installed"

augur_log "checking the result"
[ ! -e "$marker" ] || { echo "the old bundle was not actually replaced" >&2; exit 1; }
[ ! -e "$staged" ] || { echo "staging directory left behind" >&2; exit 1; }
[ ! -e "$retired" ] || { echo "retired bundle left behind" >&2; exit 1; }

codesign --verify --deep --strict "$installed"
archs="$(lipo -archs "$installed/Contents/MacOS/AugurRS")"
echo "  architectures: $archs"
for arch in arm64 x86_64; do
  case " $archs " in
    *" $arch "*) ;;
    *) echo "the swapped bundle lost $arch" >&2; exit 1 ;;
  esac
done

# Deliberately not launched: AugurRS is a GUI binary with no --version, so
# running it would open a window and block forever. `codesign --verify --deep
# --strict` above is the check that matters - it is exactly what the kernel
# evaluates at launch, and it is what fails when a bundle is copied or swapped
# incorrectly.
augur_log "confirming the signature survived the swap"
# Captured rather than piped into `grep -q`: grep exits on its first match,
# SIGPIPEs codesign, and `set -o pipefail` then reports the pipeline as failed
# even though the match succeeded.
signature="$(codesign -dv "$installed" 2>&1)"
case "$signature" in
  *Signature=adhoc*) ;;
  *) echo "the swapped bundle lost its ad-hoc signature" >&2; exit 1 ;;
esac
/usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" "$installed/Contents/Info.plist" >/dev/null

augur_log "macOS update swap verified"
