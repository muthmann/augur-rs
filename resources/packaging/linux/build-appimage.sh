#!/usr/bin/env bash
#
# Build the Linux release artifacts:
#
#   AugurRS-<version>-linux-x86_64.AppImage   GUI install and update payload
#   augur-<version>-linux-x86_64.tar.gz       CLI/terminal archive
#
# An AppImage is a single executable file that runs on any reasonably current
# glibc distro without an install step. That is also what makes it the right
# update payload: applying an update is replacing exactly one file, with no
# package manager to fight and no root needed.
#
# usage: build-appimage.sh [--out DIR] [--skip-build]

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

repo_root="$(packaging_repo_root)"
out_dir="$repo_root/dist"
skip_build=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out_dir="$2"; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

version="$(augur_version)"
release_dir="$repo_root/target/release"
tools_dir="$repo_root/target/appimage-tools"

cd "$repo_root"
out_dir="$(augur_abs_out_dir "$out_dir")"
mkdir -p "$tools_dir"

if [ "$skip_build" -eq 0 ]; then
  augur_log "building release binaries"
  cargo build --release --locked --bin augur --bin AugurRS
fi

augur_log "fetching linuxdeploy"
linuxdeploy="$tools_dir/linuxdeploy-x86_64.AppImage"
if [ ! -x "$linuxdeploy" ]; then
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$linuxdeploy" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
  chmod +x "$linuxdeploy"
fi

augur_log "staging AppDir"
appdir="$repo_root/target/AppDir"
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin"
cp "$release_dir/AugurRS" "$release_dir/augur" "$appdir/usr/bin/"

# linuxdeploy matches the icon to the desktop file's `Icon=` key by basename,
# so the file it is handed has to be named exactly `AugurRS.png`. The committed
# asset keeps its size in the name, so stage a correctly named copy.
icon_stage="$repo_root/target/appimage-icon"
rm -rf "$icon_stage"
mkdir -p "$icon_stage"
cp "$repo_root/assets/AugurRS-256.png" "$icon_stage/AugurRS.png"

# GitHub runners have no FUSE, so both linuxdeploy and the appimagetool it
# invokes have to unpack themselves instead of mounting.
export APPIMAGE_EXTRACT_AND_RUN=1
export OUTPUT="AugurRS-$version-linux-x86_64.AppImage"
export VERSION="$version"

augur_log "building AppImage"
rm -f "$repo_root/$OUTPUT" "$out_dir/$OUTPUT"
"$linuxdeploy" \
  --appdir "$appdir" \
  --executable "$appdir/usr/bin/AugurRS" \
  --desktop-file "$repo_root/resources/packaging/linux/AugurRS.desktop" \
  --icon-file "$icon_stage/AugurRS.png" \
  --output appimage

mv "$repo_root/$OUTPUT" "$out_dir/$OUTPUT"
chmod +x "$out_dir/$OUTPUT"

augur_log "packaging CLI archive"
cli_stage="$out_dir/augur-linux"
rm -rf "$cli_stage"
mkdir -p "$cli_stage/bin" "$cli_stage/examples"
cp "$release_dir/augur" "$release_dir/AugurRS" "$cli_stage/bin/"
while read -r doc; do cp "$repo_root/$doc" "$cli_stage/"; done < <(augur_doc_files)
cp "$repo_root/examples/augur.toml" "$cli_stage/examples/"
cp "$repo_root/resources/packaging/linux/AugurRS.desktop" "$cli_stage/"
cp "$repo_root/assets/AugurRS-256.png" "$cli_stage/AugurRS.png"
tar_name="augur-$version-linux-x86_64.tar.gz"
rm -f "$out_dir/$tar_name"
tar -czf "$out_dir/$tar_name" -C "$out_dir" augur-linux
rm -rf "$cli_stage"

augur_log "artifacts in $out_dir"
ls -lh "$out_dir/$OUTPUT" "$out_dir/$tar_name"
