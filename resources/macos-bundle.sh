#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <augur-gui-binary> <output-dir>" >&2
  exit 1
fi

binary_path="$1"
output_dir="$2"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
icon_source="$repo_root/assets/logo.png"
bundle_icon_name="AugurGUI.icns"

if [ ! -f "$binary_path" ]; then
  echo "missing binary: $binary_path" >&2
  exit 1
fi

version="$(awk -F'\"' '/^version = / { print $2; exit }' "$repo_root/Cargo.toml")"
if [ -z "$version" ]; then
  echo "failed to read workspace version from Cargo.toml" >&2
  exit 1
fi

if [ ! -f "$icon_source" ]; then
  echo "missing icon source: $icon_source" >&2
  exit 1
fi

if ! command -v sips >/dev/null 2>&1 || ! command -v iconutil >/dev/null 2>&1; then
  echo "missing required macOS icon tools: sips and iconutil" >&2
  exit 1
fi

generate_bundle_icon() {
  local temp_dir iconset_dir size retina

  temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/augur-icon.XXXXXX")"
  iconset_dir="$temp_dir/AugurGUI.iconset"
  mkdir -p "$iconset_dir"

  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon_source" \
      --out "$iconset_dir/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" "$icon_source" \
      --out "$iconset_dir/icon_${size}x${size}@2x.png" >/dev/null
  done

  iconutil -c icns "$iconset_dir" -o "$resources_dir/$bundle_icon_name"
  rm -rf "$temp_dir"
}

app_dir="$output_dir/AugurGUI.app"
contents_dir="$app_dir/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"

rm -rf "$app_dir"
mkdir -p "$macos_dir" "$resources_dir"
cp "$binary_path" "$macos_dir/augur-gui"
chmod +x "$macos_dir/augur-gui"
generate_bundle_icon
sed "s/__AUGUR_VERSION__/$version/g" "$repo_root/resources/Info.plist" > "$contents_dir/Info.plist"
