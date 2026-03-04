# Release And Distribution Notes

## Current State

Today the repository ships in three layers:

- developers can build from source with Cargo
- GitHub Actions can verify repository health automatically
- tagged releases can publish prebuilt macOS artifacts

## Non-Technical Users

For non-technical users, the long-term goal should be:

- downloadable GUI releases from GitHub Releases
- minimal manual setup
- a proper desktop app package for macOS

This repository now packages an unsigned `.app` bundle, but it is not fully turnkey yet.

## What The Release Workflow Provides

The GitHub Actions release workflow is intended to:

- build `augur` and `augur-gui` on tagged versions
- package a source-first `augur-macos.zip` archive with binaries, docs, and example config
- assemble `AugurGUI.app` and publish it as `AugurGUI.app.zip`
- attach both archives to GitHub Releases

## What It Does Not Solve Yet

- macOS code signing
- macOS notarization
- installer-based distribution

Those are the next steps if the project should feel truly turnkey for GUI-only users.

## Practical Recommendation

For now:

- technical users can build from source immediately
- less technical users can start from release artifacts once tags are published
- if macOS GUI distribution becomes a priority, add signed `.app` packaging next
