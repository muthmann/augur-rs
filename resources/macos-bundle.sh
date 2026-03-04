#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <augur-gui-binary> <output-dir>" >&2
  exit 1
fi

binary_path="$1"
output_dir="$2"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"

if [ ! -f "$binary_path" ]; then
  echo "missing binary: $binary_path" >&2
  exit 1
fi

version="$(awk -F'\"' '/^version = / { print $2; exit }' "$repo_root/Cargo.toml")"
if [ -z "$version" ]; then
  echo "failed to read workspace version from Cargo.toml" >&2
  exit 1
fi

app_dir="$output_dir/AugurGUI.app"
contents_dir="$app_dir/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"

rm -rf "$app_dir"
mkdir -p "$macos_dir" "$resources_dir"
cp "$binary_path" "$macos_dir/augur-gui"
chmod +x "$macos_dir/augur-gui"
sed "s/__AUGUR_VERSION__/$version/g" "$repo_root/resources/Info.plist" > "$contents_dir/Info.plist"
