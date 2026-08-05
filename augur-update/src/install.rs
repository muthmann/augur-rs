//! Applying a verified payload, per platform.
//!
//! Every path here either completes or leaves the existing installation
//! untouched. Nothing writes into a location the current user does not already
//! own, so a package-manager-owned or root-owned install is reported as
//! unsupported rather than half-modified.

use std::path::{Path, PathBuf};

use crate::target::PayloadKind;
use crate::{Download, UpdateError};

/// What the caller has to do once the update has been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The new build is in place and has been started; this process should exit.
    Relaunched,
    /// An installer is running and will replace files once this process exits,
    /// so the caller must quit promptly.
    InstallerRunning,
}

pub fn apply(download: &Download) -> Result<Applied, UpdateError> {
    match download.kind {
        PayloadKind::MacosDiskImage => apply_macos(&download.path),
        PayloadKind::WindowsInstaller => apply_windows(&download.path),
        PayloadKind::LinuxAppImage => apply_linux(&download.path),
    }
}

/// Where this build is installed, described well enough to tell the user why an
/// update is or is not possible before anything is downloaded.
pub fn install_kind() -> Result<PayloadKind, UpdateError> {
    let target = crate::target::current().ok_or(UpdateError::UnsupportedPlatform)?;

    match target.kind {
        PayloadKind::MacosDiskImage => {
            macos_bundle_path()?;
            Ok(target.kind)
        }
        PayloadKind::LinuxAppImage => {
            appimage_path()?;
            Ok(target.kind)
        }
        PayloadKind::WindowsInstaller => Ok(target.kind),
    }
}

// ---------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
fn macos_bundle_path() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(UpdateError::Io)?;

    // .../AugurRS.app/Contents/MacOS/AugurRS
    let bundle = exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or(UpdateError::NotInstalled(
            "running outside an application bundle",
        ))?
        .to_path_buf();

    // Swapping the bundle means writing into its parent directory, so that is
    // the permission that actually matters - not the bundle's own mode.
    let parent = bundle.parent().ok_or(UpdateError::NotInstalled(
        "application bundle has no parent directory",
    ))?;
    if is_read_only(parent) {
        return Err(UpdateError::NotWritable(parent.to_path_buf()));
    }

    Ok(bundle)
}

#[cfg(not(target_os = "macos"))]
fn macos_bundle_path() -> Result<PathBuf, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(target_os = "macos")]
fn apply_macos(dmg: &Path) -> Result<Applied, UpdateError> {
    use std::process::Command;

    let installed = macos_bundle_path()?;
    let mount = tempdir("augur-update-mount")?;

    run(
        Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
            .arg(&mount)
            .arg(dmg),
        "mount the disk image",
    )?;

    let result = swap_macos_bundle(&mount, &installed);

    // Detach whether or not the swap worked; a leaked mount would block the
    // next attempt at the same mountpoint.
    let _ = run(
        Command::new("hdiutil")
            .arg("detach")
            .arg(&mount)
            .arg("-quiet"),
        "unmount the disk image",
    );
    let _ = std::fs::remove_dir(&mount);

    result?;

    // Re-apply the ad-hoc signature. Copying the bundle preserves it, but the
    // swap changes the path it was sealed at, and macOS refuses to launch a
    // bundle whose signature no longer validates.
    run(
        Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&installed),
        "re-sign the updated bundle",
    )?;

    run(
        Command::new("open").arg("-n").arg(&installed),
        "relaunch the updated application",
    )?;

    Ok(Applied::Relaunched)
}

#[cfg(target_os = "macos")]
fn swap_macos_bundle(mount: &Path, installed: &Path) -> Result<(), UpdateError> {
    use std::process::Command;

    let source = std::fs::read_dir(mount)
        .map_err(UpdateError::Io)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or(UpdateError::MalformedPayload(
            "the disk image contains no application bundle",
        ))?;

    let staged = sibling(installed, ".update-staged");
    let retired = sibling(installed, ".update-old");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&retired);

    // ditto rather than a manual walk: it is the only copy on macOS that
    // preserves extended attributes and the signature's resource fork.
    run(
        Command::new("ditto").arg(&source).arg(&staged),
        "copy the new application bundle",
    )?;

    // Move the old bundle aside before moving the new one in, so a failure
    // halfway through leaves a complete bundle at one path or the other rather
    // than a partially overwritten one.
    std::fs::rename(installed, &retired).map_err(UpdateError::Io)?;
    if let Err(error) = std::fs::rename(&staged, installed) {
        let _ = std::fs::rename(&retired, installed);
        return Err(UpdateError::Io(error));
    }
    let _ = std::fs::remove_dir_all(&retired);

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apply_macos(_dmg: &Path) -> Result<Applied, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

// -------------------------------------------------------------- Windows

#[cfg(target_os = "windows")]
fn apply_windows(installer: &Path) -> Result<Applied, UpdateError> {
    use std::process::Command;

    // The NSIS installer is per-user, so /S completes without a UAC prompt. It
    // stops the running AugurRS.exe itself before replacing files, which is why
    // this returns rather than trying to sequence the shutdown from here.
    Command::new(installer)
        .arg("/S")
        .spawn()
        .map_err(UpdateError::Io)?;

    Ok(Applied::InstallerRunning)
}

#[cfg(not(target_os = "windows"))]
fn apply_windows(_installer: &Path) -> Result<Applied, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

// ---------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
fn appimage_path() -> Result<PathBuf, UpdateError> {
    // The AppImage runtime exports $APPIMAGE as the path of the image itself.
    // current_exe() inside an AppImage points into the extracted mount, which
    // disappears on exit, so it is the wrong thing to replace.
    let path = std::env::var_os("APPIMAGE").ok_or(UpdateError::NotInstalled(
        "not running from an AppImage - reinstall from the release page instead",
    ))?;
    let path = PathBuf::from(path);

    let parent = path.parent().ok_or(UpdateError::NotInstalled(
        "AppImage has no parent directory",
    ))?;
    if is_read_only(parent) {
        return Err(UpdateError::NotWritable(parent.to_path_buf()));
    }

    Ok(path)
}

#[cfg(not(target_os = "linux"))]
fn appimage_path() -> Result<PathBuf, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn apply_linux(payload: &Path) -> Result<Applied, UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let installed = appimage_path()?;

    // Stage beside the target so the final step is a rename within one
    // filesystem, which is atomic. A rename also sidesteps ETXTBSY: the running
    // process keeps the old inode while the directory entry moves on.
    let staged = sibling(&installed, ".update-new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(payload, &staged).map_err(UpdateError::Io)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .map_err(UpdateError::Io)?;
    std::fs::rename(&staged, &installed).map_err(UpdateError::Io)?;

    Command::new(&installed).spawn().map_err(UpdateError::Io)?;

    Ok(Applied::Relaunched)
}

#[cfg(not(target_os = "linux"))]
fn apply_linux(_payload: &Path) -> Result<Applied, UpdateError> {
    Err(UpdateError::UnsupportedPlatform)
}

// --------------------------------------------------------------- shared

/// `path` with `suffix` appended to its file name, keeping it in the same
/// directory so renames stay within one filesystem.
#[allow(dead_code)]
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[allow(dead_code)]
fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().readonly())
        || probe_write(path).is_err()
}

/// Permission bits alone do not answer "can I write here" - a read-only mount
/// or a restrictive ACL does not show up in them. Creating and removing a file
/// is the only honest test, and it is cheap next to a download.
#[allow(dead_code)]
fn probe_write(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".augur-update-probe-{}", std::process::id()));
    std::fs::File::create(&probe)?;
    std::fs::remove_file(&probe)
}

#[allow(dead_code)]
fn tempdir(prefix: &str) -> Result<PathBuf, UpdateError> {
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        crate::monotonic_suffix()
    ));
    std::fs::create_dir_all(&path).map_err(UpdateError::Io)?;
    Ok(path)
}

#[allow(dead_code)]
fn run(command: &mut std::process::Command, what: &'static str) -> Result<(), UpdateError> {
    let output = command.output().map_err(UpdateError::Io)?;
    if output.status.success() {
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(UpdateError::CommandFailed { what, detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_stays_in_the_same_directory() {
        let path = Path::new("/opt/apps/AugurRS.AppImage");
        let staged = sibling(path, ".update-new");

        assert_eq!(staged.parent(), path.parent());
        assert_eq!(staged.file_name().unwrap(), "AugurRS.AppImage.update-new");
    }

    #[test]
    fn write_probe_leaves_nothing_behind() {
        let dir = std::env::temp_dir();
        probe_write(&dir).expect("temp dir should be writable");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".augur-update-probe-")
            })
            .collect();
        assert!(leftovers.is_empty(), "probe file was not cleaned up");
    }

    #[test]
    fn read_only_locations_are_rejected() {
        // A path that does not exist cannot be written to, and must not be
        // mistaken for a writable one just because metadata() failed.
        assert!(is_read_only(Path::new(
            "/definitely-not-a-real-directory-augur-update"
        )));
    }
}
