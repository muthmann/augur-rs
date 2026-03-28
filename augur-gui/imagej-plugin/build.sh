#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_JAR="$SCRIPT_DIR/AugurBridge.jar"

find_imagej_jar() {
  if [[ -n "${IMAGEJ_JAR:-}" ]]; then
    printf '%s\n' "$IMAGEJ_JAR"
    return 0
  fi

  local candidates=(
    "$SCRIPT_DIR/ij.jar"
    "/Applications/Fiji.app/jars/ij.jar"
    "/Applications/Fiji.app/Contents/Java/ij.jar"
    "/Applications/ImageJ.app/Contents/Java/ij.jar"
    "$HOME/Fiji.app/jars/ij.jar"
    "$HOME/ImageJ/ij.jar"
  )

  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

IMAGEJ_JAR_PATH="$(find_imagej_jar || true)"
if [[ -z "$IMAGEJ_JAR_PATH" ]]; then
  echo "Could not find ij.jar." >&2
  echo "Set IMAGEJ_JAR=/path/to/ij.jar and re-run this script." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

mkdir -p "$TMP_DIR/classes"

javac --release 8 \
  -cp "$IMAGEJ_JAR_PATH" \
  -d "$TMP_DIR/classes" \
  "$SCRIPT_DIR/AugurBridge.java"

jar cf "$OUTPUT_JAR" \
  -C "$TMP_DIR/classes" . \
  -C "$SCRIPT_DIR" plugins.config

echo "Built $OUTPUT_JAR"
echo "Using ij.jar at $IMAGEJ_JAR_PATH"
