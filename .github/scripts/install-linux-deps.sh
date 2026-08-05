#!/usr/bin/env bash
#
# System packages needed to build (and link) the AugurRS GUI on Linux.
#
# Shared by the CI matrix and the release job so the two can never drift — a
# release build failing on a library CI never installed is exactly the kind of
# breakage that only shows up once a tag has already been pushed.

set -euo pipefail

sudo apt-get update

sudo apt-get install -y --no-install-recommends \
  pkg-config \
  libx11-dev \
  libxi-dev \
  libxcursor-dev \
  libxrandr-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libwayland-dev \
  libgl1-mesa-dev \
  libegl1-mesa-dev \
  libudev-dev
