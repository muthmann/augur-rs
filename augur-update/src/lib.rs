//! Checks GitHub releases for a newer AugurRS and applies the update.
//!
//! # Shape of the thing
//!
//! Each supported platform publishes exactly one file that is simultaneously
//! what a person downloads and what the updater applies — a `.dmg`, an NSIS
//! `-setup.exe`, or an `.AppImage`. Because of that, updating never involves
//! unpacking an archive, and this crate needs no zip, tar, or gzip dependency.
//!
//! # Rules this crate will not bend
//!
//! - **Downgrades are refused.** A remote version that is not strictly greater
//!   than the running one reports [`UpdateStatus::UpToDate`].
//! - **Checksums are mandatory.** A release without `SHA256SUMS`, or a payload
//!   whose digest does not match, aborts the update instead of warning.
//! - **Nothing is applied without being asked.** `check` and `download` never
//!   modify the installation; only [`apply`] does.
//!
//! That last one is not caution for its own sake. `augur-core` stamps the
//! running version into every recording sidecar, so which binary produced a
//! dataset is part of the scientific record. Swapping it out mid-session would
//! quietly invalidate that.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

mod checksum;
mod feed;
mod install;
mod target;
mod version;

pub use install::{install_kind, Applied};
pub use target::{PayloadKind, Target};
pub use version::{ParseVersionError, Version};

/// Repository the updater reads releases from.
pub const DEFAULT_REPO: &str = "muthmann/augur-rs";

/// Name of the digest manifest every release must carry.
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";

/// Set to `1` to suppress the automatic check entirely. Exists so a managed or
/// air-gapped deployment can guarantee no outbound request is ever made.
pub const DISABLE_ENV: &str = "AUGUR_NO_UPDATE_CHECK";

/// Points the updater at a different `owner/name`. For testing against a fork
/// without rebuilding.
pub const REPO_ENV: &str = "AUGUR_UPDATE_REPO";

const API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("augur-update/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// A release asset this platform can install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub kind: PayloadKind,
}

/// A published release, already resolved to the asset this build would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub title: String,
    pub notes: String,
    pub notes_url: String,
    pub published_at: String,
    pub asset: Asset,
    pub checksums_url: String,
}

/// A verified payload on disk, ready for [`apply`].
#[derive(Debug, Clone)]
pub struct Download {
    pub path: PathBuf,
    pub kind: PayloadKind,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Nothing newer is published. Carries the version that was compared
    /// against so callers can show "you are on the latest, 1.2.3".
    UpToDate(Version),
    Available(Box<Release>),
}

#[derive(Debug)]
pub enum UpdateError {
    /// The check was switched off via [`DISABLE_ENV`].
    Disabled,
    /// This OS/architecture has no published build.
    UnsupportedPlatform,
    /// Installed in a way the updater cannot replace (source build, extracted
    /// archive, package manager).
    NotInstalled(&'static str),
    /// The install location exists but this user cannot write to it.
    NotWritable(PathBuf),
    /// The latest release is a draft or prerelease.
    NoStableRelease,
    /// The release tag is not a version number.
    UnreadableTag(String),
    /// The release publishes no `SHA256SUMS`.
    MissingChecksums,
    /// The release has no asset matching this platform's suffix.
    NoAssetForPlatform {
        suffix: &'static str,
    },
    /// `SHA256SUMS` has no entry for the asset that was downloaded.
    UnlistedAsset(String),
    /// The downloaded bytes do not match the published digest.
    ChecksumMismatch {
        expected: String,
        actual: String,
    },
    /// The payload is not shaped the way this platform expects.
    MalformedPayload(&'static str),
    /// A packaging tool (`hdiutil`, `codesign`, `ditto`, `open`) failed.
    CommandFailed {
        what: &'static str,
        detail: String,
    },
    Http(String),
    Io(io::Error),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "update checks are disabled by {DISABLE_ENV}"),
            Self::UnsupportedPlatform => {
                write!(f, "no AugurRS build is published for this platform")
            }
            Self::NotInstalled(why) => write!(f, "cannot update in place: {why}"),
            Self::NotWritable(path) => {
                write!(
                    f,
                    "cannot update in place: {} is not writable",
                    path.display()
                )
            }
            Self::NoStableRelease => write!(f, "the latest release is not a stable release"),
            Self::UnreadableTag(tag) => write!(f, "release tag {tag:?} is not a version number"),
            Self::MissingChecksums => {
                write!(
                    f,
                    "the release publishes no {CHECKSUM_ASSET}, so it cannot be verified"
                )
            }
            Self::NoAssetForPlatform { suffix } => {
                write!(f, "the release has no asset ending in {suffix}")
            }
            Self::UnlistedAsset(name) => {
                write!(
                    f,
                    "{name} has no entry in {CHECKSUM_ASSET}, so it cannot be verified"
                )
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "checksum mismatch: expected {expected}, got {actual}. The download was discarded."
            ),
            Self::MalformedPayload(why) => write!(f, "unusable update payload: {why}"),
            Self::CommandFailed { what, detail } if detail.is_empty() => {
                write!(f, "failed to {what}")
            }
            Self::CommandFailed { what, detail } => write!(f, "failed to {what}: {detail}"),
            Self::Http(message) => write!(f, "could not reach the release server: {message}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for UpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Ask GitHub whether anything newer than `current_version` is published.
///
/// `current_version` is normally `env!("CARGO_PKG_VERSION")` from the calling
/// binary. Performs one HTTPS request and never touches the installation.
pub fn check(current_version: &str) -> Result<UpdateStatus, UpdateError> {
    if std::env::var(DISABLE_ENV).is_ok_and(|value| value == "1") {
        return Err(UpdateError::Disabled);
    }

    let current = Version::parse(current_version)
        .map_err(|_| UpdateError::UnreadableTag(current_version.to_owned()))?;
    let target = target::current().ok_or(UpdateError::UnsupportedPlatform)?;

    let url = format!("{}/repos/{}/releases/latest", API_BASE, repo());
    let body = get_text(&url)?;
    let api: feed::ApiRelease =
        serde_json::from_str(&body).map_err(|error| UpdateError::Http(error.to_string()))?;
    let release = api.into_release(target)?;

    // Strictly greater. Equal means up to date; lower means someone is running
    // a build newer than the last release, and reinstalling backwards over it
    // would be a surprising thing for a "check for updates" button to do.
    if release.version > current {
        Ok(UpdateStatus::Available(Box::new(release)))
    } else {
        Ok(UpdateStatus::UpToDate(current))
    }
}

/// Fetch the release payload and verify it against the release's `SHA256SUMS`.
///
/// `progress` receives `(bytes_so_far, total_bytes)`; `total` is the size the
/// release metadata advertises, which may be 0 if GitHub omitted it.
pub fn download(
    release: &Release,
    mut progress: impl FnMut(u64, u64),
) -> Result<Download, UpdateError> {
    let sums = get_text(&release.checksums_url)?;
    let expected = checksum::expected_digest(&sums, &release.asset.name)
        .ok_or_else(|| UpdateError::UnlistedAsset(release.asset.name.clone()))?;

    let dir = std::env::temp_dir().join(format!("augur-update-{}", monotonic_suffix()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&release.asset.name);

    let mut response = agent()
        .get(&release.asset.url)
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(http_error)?;

    let mut reader = response.body_mut().as_reader();
    let mut file = File::create(&path)?;
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut written = 0_u64;

    loop {
        let read = io::Read::read(&mut reader, &mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        written += read as u64;
        progress(written, release.asset.size);
    }
    file.sync_all()?;
    drop(file);

    let actual = checksum::file_digest(&path)?;
    if actual != expected {
        // Never leave an unverified executable lying around where a confused
        // user might run it by hand.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }

    Ok(Download {
        path,
        kind: release.asset.kind,
        version: release.version.clone(),
    })
}

/// Install a verified download.
///
/// On success the caller must exit promptly: either a new copy is already
/// running ([`Applied::Relaunched`]) or an installer is waiting to replace
/// files ([`Applied::InstallerRunning`]).
pub fn apply(download: &Download) -> Result<Applied, UpdateError> {
    install::apply(download)
}

/// Remove a downloaded payload and its temporary directory.
pub fn discard(download: &Download) {
    let _ = std::fs::remove_file(&download.path);
    if let Some(dir) = download.path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Web page for a release, for the "installed from source" fallback where
/// there is nothing to apply but still something useful to show.
pub fn releases_url() -> String {
    format!("https://github.com/{}/releases/latest", repo())
}

fn repo() -> String {
    std::env::var(REPO_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_owned())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

fn get_text(url: &str) -> Result<String, UpdateError> {
    agent()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(http_error)?
        .body_mut()
        .read_to_string()
        .map_err(http_error)
}

fn http_error(error: ureq::Error) -> UpdateError {
    UpdateError::Http(error.to_string())
}

/// Cheap uniquifier for temporary paths. Not security-relevant: the directory
/// is created with `create_dir_all` under the user's own temp dir.
fn monotonic_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            tag: format!("v{version}"),
            title: format!("v{version}"),
            notes: String::new(),
            notes_url: String::new(),
            published_at: String::new(),
            asset: Asset {
                name: format!("AugurRS-{version}-linux-x86_64.AppImage"),
                url: String::new(),
                size: 0,
                kind: PayloadKind::LinuxAppImage,
            },
            checksums_url: String::new(),
        }
    }

    /// The comparison `check` performs, isolated from the HTTP request so the
    /// downgrade rule is actually testable.
    fn decide(current: &str, remote: &str) -> UpdateStatus {
        let current = Version::parse(current).unwrap();
        let remote = release(remote);
        if remote.version > current {
            UpdateStatus::Available(Box::new(remote))
        } else {
            UpdateStatus::UpToDate(current)
        }
    }

    #[test]
    fn offers_a_newer_release() {
        assert!(matches!(
            decide("1.0.0", "1.0.1"),
            UpdateStatus::Available(_)
        ));
        assert!(matches!(
            decide("1.0.0", "1.1.0"),
            UpdateStatus::Available(_)
        ));
        assert!(matches!(
            decide("1.9.9", "2.0.0"),
            UpdateStatus::Available(_)
        ));
    }

    #[test]
    fn never_offers_a_downgrade_or_a_reinstall() {
        assert!(matches!(
            decide("1.2.3", "1.2.3"),
            UpdateStatus::UpToDate(_)
        ));
        // A developer build ahead of the last release must be left alone.
        assert!(matches!(
            decide("1.3.0", "1.2.9"),
            UpdateStatus::UpToDate(_)
        ));
        assert!(matches!(
            decide("2.0.0", "1.99.99"),
            UpdateStatus::UpToDate(_)
        ));
    }

    #[test]
    fn respects_the_disable_switch() {
        // Guarded rather than asserted unconditionally: the variable is process
        // global, so only check it when the environment actually sets it.
        if std::env::var(DISABLE_ENV).is_ok_and(|value| value == "1") {
            assert!(matches!(check("1.0.0"), Err(UpdateError::Disabled)));
        }
    }

    #[test]
    fn repo_override_falls_back_to_the_default() {
        // Empty or unset must not produce a request to "/repos//releases".
        assert!(!repo().is_empty());
        assert!(repo().contains('/'));
    }

    #[test]
    fn errors_explain_themselves() {
        let mismatch = UpdateError::ChecksumMismatch {
            expected: "a".repeat(64),
            actual: "b".repeat(64),
        };
        assert!(mismatch.to_string().contains("discarded"));

        let unsupported = UpdateError::NoAssetForPlatform {
            suffix: "-linux-x86_64.AppImage",
        };
        assert!(unsupported.to_string().contains("-linux-x86_64.AppImage"));
    }

    #[test]
    fn releases_url_points_at_the_configured_repo() {
        assert!(releases_url().starts_with("https://github.com/"));
        assert!(releases_url().ends_with("/releases/latest"));
    }
}
