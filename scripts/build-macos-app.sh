#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
usage: build-macos-app.sh [--install] [--install-dir DIR] [--dmg] [--output-dir DIR]

Builds a local AugurRS macOS app bundle from source.

Options:
  --install            Copy the built app into the install directory after bundling.
  --install-dir DIR    Destination for --install. Defaults to /Applications.
  --dmg                Also create a local AugurRS.dmg next to the app bundle.
  --output-dir DIR     Bundle output directory. Defaults to dist/local-macos.
  -h, --help           Show this help text.
EOF
}

if [[ "${OSTYPE:-}" != darwin* ]]; then
  echo "build-macos-app.sh must be run on macOS." >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
output_dir="$repo_root/dist/local-macos"
install_app=false
install_dir="/Applications"
create_dmg=false

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --install)
      install_app=true
      shift
      ;;
    --install-dir)
      if [[ "$#" -lt 2 ]]; then
        echo "missing value for --install-dir" >&2
        exit 1
      fi
      install_dir="$2"
      shift 2
      ;;
    --dmg)
      create_dmg=true
      shift
      ;;
    --output-dir)
      if [[ "$#" -lt 2 ]]; then
        echo "missing value for --output-dir" >&2
        exit 1
      fi
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

binary_path="$repo_root/target/release/AugurRS"
app_path="$output_dir/AugurRS.app"
dmg_path="$output_dir/AugurRS.dmg"

mkdir -p "$output_dir"

(
  cd "$repo_root"
  cargo build --release --locked --bin AugurRS
  bash resources/macos-bundle.sh "$binary_path" "$output_dir"
  # Rust's Mach-O carries a linker signature for the standalone binary. Once
  # it is placed beside Info.plist and the icon that signature no longer seals
  # the complete app bundle, so Finder/Gatekeeper rejects the local artifact.
  # Re-sign the finished bundle exactly as the release packaging path does.
  codesign --force --deep --sign - "$app_path"
  codesign --verify --deep --strict "$app_path"
)

if $install_app; then
  mkdir -p "$install_dir"
  rm -rf "$install_dir/AugurRS.app"
  ditto "$app_path" "$install_dir/AugurRS.app"
  echo "Installed AugurRS.app to $install_dir"
fi

if $create_dmg; then
  staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/augur-dmg.XXXXXX")"
  trap 'rm -rf "$staging_dir"' EXIT
  cp -R "$app_path" "$staging_dir/"
  ln -s /Applications "$staging_dir/Applications"
  rm -f "$dmg_path"
  hdiutil create \
    -volname "AugurRS" \
    -srcfolder "$staging_dir" \
    -ov -format UDZO \
    "$dmg_path" >/dev/null
  echo "Created local DMG at $dmg_path"
fi

echo "Built app bundle at $app_path"
