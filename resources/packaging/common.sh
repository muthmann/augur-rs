#!/usr/bin/env bash
#
# Shared helpers for the platform packaging scripts.
#
# Artifact names are derived here and nowhere else. The in-app updater selects
# its download by matching these exact names, so a rename made in one place and
# not the other silently breaks updating for everyone already on the old build.

set -euo pipefail

packaging_repo_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd
}

# Workspace version from `[workspace.package] version`, which release-please
# bumps. The first bare `version = ` line in the root Cargo.toml is that field.
augur_version() {
  local root version
  root="$(packaging_repo_root)"
  version="$(awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml")"
  if [ -z "$version" ]; then
    echo "failed to read workspace version from $root/Cargo.toml" >&2
    return 1
  fi
  printf '%s' "$version"
}

# Files every distribution archive carries so a download is self-describing.
augur_doc_files() {
  printf '%s\n' README.md LICENSE CONTRIBUTING.md CHANGELOG.md
}

augur_log() {
  printf '\033[1;36m==>\033[0m %s\n' "$*" >&2
}
