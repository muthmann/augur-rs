//! Which release asset this build should download, and what to do with it.
//!
//! Every supported platform is reduced to exactly one file that is both the
//! thing a human downloads and the thing the updater applies. That is what
//! keeps the updater free of archive handling: there is nothing to unpack.
//!
//! The suffixes below must match the names produced by `resources/packaging/`.
//! If those two drift apart, `check` reports `NoAssetForPlatform` rather than
//! guessing, so the failure is loud instead of silently installing the wrong
//! architecture.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// Disk image holding `AugurRS.app`; applied by swapping the bundle.
    MacosDiskImage,
    /// NSIS installer; applied by re-running it silently.
    WindowsInstaller,
    /// Self-contained executable; applied by replacing the file in place.
    LinuxAppImage,
}

impl fmt::Display for PayloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::MacosDiskImage => "macOS disk image",
            Self::WindowsInstaller => "Windows installer",
            Self::LinuxAppImage => "Linux AppImage",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Release assets are matched by this filename suffix.
    pub asset_suffix: &'static str,
    pub kind: PayloadKind,
}

/// The asset this build can install, or `None` on a platform the release
/// pipeline does not publish for (32-bit, aarch64 Linux, BSD, ...).
pub const fn current() -> Option<Target> {
    #[cfg(target_os = "macos")]
    {
        // One universal disk image covers both Apple Silicon and Intel, so the
        // architecture this binary happens to be running as does not matter.
        Some(Target {
            asset_suffix: "-macos-universal.dmg",
            kind: PayloadKind::MacosDiskImage,
        })
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(Target {
            asset_suffix: "-windows-x86_64-setup.exe",
            kind: PayloadKind::WindowsInstaller,
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(Target {
            asset_suffix: "-linux-x86_64.AppImage",
            kind: PayloadKind::LinuxAppImage,
        })
    }

    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_match_the_packaging_scripts() {
        // These strings are the contract with resources/packaging/. Changing a
        // filename there without changing it here strands every installed copy,
        // because an older build looks for the name it was compiled with.
        let Some(target) = current() else {
            return; // unsupported platform: nothing to assert
        };

        assert!(target.asset_suffix.starts_with('-'));
        let expected = match target.kind {
            PayloadKind::MacosDiskImage => "-macos-universal.dmg",
            PayloadKind::WindowsInstaller => "-windows-x86_64-setup.exe",
            PayloadKind::LinuxAppImage => "-linux-x86_64.AppImage",
        };
        assert_eq!(target.asset_suffix, expected);
    }
}
