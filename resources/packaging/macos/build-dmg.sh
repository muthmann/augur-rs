#!/usr/bin/env bash
#
# Build the macOS release artifacts:
#
#   AugurRS-<version>-macos-universal.dmg     GUI installer and update payload
#   augur-<version>-macos-universal.tar.gz    CLI/terminal archive
#
# Both carry universal binaries. `macos-latest` runners are Apple Silicon, so a
# single-arch build silently shipped an executable Intel Macs cannot run.
#
# usage: build-dmg.sh [--out DIR] [--skip-build]

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

repo_root="$(packaging_repo_root)"
out_dir="$repo_root/dist"
skip_build=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out_dir="$2"; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

version="$(augur_version)"
targets=(aarch64-apple-darwin x86_64-apple-darwin)
universal_dir="$repo_root/target/universal-apple-darwin/release"

cd "$repo_root"
out_dir="$(augur_abs_out_dir "$out_dir")"

if [ "$skip_build" -eq 0 ]; then
  for target in "${targets[@]}"; do
    augur_log "building $target"
    rustup target add "$target" >/dev/null
    cargo build --release --locked --target "$target" --bin augur --bin AugurRS
  done
fi

augur_log "creating universal binaries"
mkdir -p "$universal_dir"
for binary in augur AugurRS; do
  lipo -create -output "$universal_dir/$binary" \
    "$repo_root/target/aarch64-apple-darwin/release/$binary" \
    "$repo_root/target/x86_64-apple-darwin/release/$binary"
  lipo -info "$universal_dir/$binary"
done

augur_log "assembling AugurRS.app"
app_stage="$out_dir/app"
rm -rf "$app_stage"
mkdir -p "$app_stage"
bash "$repo_root/resources/macos-bundle.sh" "$universal_dir/AugurRS" "$app_stage"

# Ad-hoc signature. Without a stable code signature macOS refuses to launch the
# bundle after anything modifies it in place — which is exactly what the in-app
# updater does when it swaps the bundle. Not a substitute for notarization:
# Gatekeeper still warns on first launch of a downloaded copy.
augur_log "ad-hoc signing the bundle"
codesign --force --deep --sign - "$app_stage/AugurRS.app"
codesign --verify --deep --strict "$app_stage/AugurRS.app"

augur_log "creating disk image"
dmg_name="AugurRS-$version-macos-universal.dmg"
dmg_stage="$(mktemp -d "${TMPDIR:-/tmp}/augur-dmg.XXXXXX")"
trap 'rm -rf "$dmg_stage"' EXIT
cp -R "$app_stage/AugurRS.app" "$dmg_stage/"
ln -s /Applications "$dmg_stage/Applications"
cp "$repo_root/resources/packaging/macos/FIRST-LAUNCH.txt" "$dmg_stage/First launch — read me.txt"
rm -f "$out_dir/$dmg_name"
hdiutil create \
  -volname "AugurRS $version" \
  -srcfolder "$dmg_stage" \
  -ov -format UDZO \
  "$out_dir/$dmg_name"

augur_log "packaging CLI archive"
cli_stage="$out_dir/augur-macos"
rm -rf "$cli_stage"
mkdir -p "$cli_stage/bin" "$cli_stage/examples"
cp "$universal_dir/augur" "$universal_dir/AugurRS" "$cli_stage/bin/"
while read -r doc; do cp "$repo_root/$doc" "$cli_stage/"; done < <(augur_doc_files)
cp "$repo_root/examples/augur.toml" "$cli_stage/examples/"
tar_name="augur-$version-macos-universal.tar.gz"
rm -f "$out_dir/$tar_name"
tar -czf "$out_dir/$tar_name" -C "$out_dir" augur-macos
rm -rf "$cli_stage"

augur_log "artifacts in $out_dir"
ls -lh "$out_dir/$dmg_name" "$out_dir/$tar_name"
