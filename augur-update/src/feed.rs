//! Reading the published release list from the GitHub REST API.

use serde::Deserialize;

use crate::target::Target;
use crate::version::Version;
use crate::{Asset, Release, UpdateError};

/// Shape of the subset of `GET /repos/{repo}/releases/latest` we rely on.
/// Unknown fields are ignored, so GitHub can extend the payload freely.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<ApiAsset>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

impl ApiRelease {
    /// Turn the API payload into the release description the rest of the crate
    /// works with, or explain precisely why this release is not installable.
    pub(crate) fn into_release(self, target: Target) -> Result<Release, UpdateError> {
        // `/releases/latest` already excludes these, but a caller pointing at a
        // custom feed should not be able to smuggle one past us.
        if self.draft || self.prerelease {
            return Err(UpdateError::NoStableRelease);
        }

        let version = Version::parse(&self.tag_name)
            .map_err(|_| UpdateError::UnreadableTag(self.tag_name.clone()))?;

        let checksums = self
            .assets
            .iter()
            .find(|asset| asset.name == crate::CHECKSUM_ASSET)
            .map(|asset| asset.browser_download_url.clone())
            .ok_or(UpdateError::MissingChecksums)?;

        let asset = self
            .assets
            .iter()
            .find(|asset| asset.name.ends_with(target.asset_suffix))
            .map(|asset| Asset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
                kind: target.kind,
            })
            .ok_or(UpdateError::NoAssetForPlatform {
                suffix: target.asset_suffix,
            })?;

        Ok(Release {
            version,
            title: self.name.unwrap_or_else(|| self.tag_name.clone()),
            tag: self.tag_name,
            notes: self.body.unwrap_or_default(),
            notes_url: self.html_url,
            published_at: self.published_at.unwrap_or_default(),
            asset,
            checksums_url: checksums,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::PayloadKind;

    const TARGET: Target = Target {
        asset_suffix: "-linux-x86_64.AppImage",
        kind: PayloadKind::LinuxAppImage,
    };

    fn payload(assets: &str, extra: &str) -> ApiRelease {
        let json = format!(
            r#"{{
                "tag_name": "v1.2.0",
                "name": "v1.2.0",
                "html_url": "https://example.invalid/releases/v1.2.0",
                "body": "notes",
                "published_at": "2026-08-01T00:00:00Z",
                {extra}
                "assets": [{assets}]
            }}"#
        );
        serde_json::from_str(&json).expect("valid fixture")
    }

    fn asset(name: &str) -> String {
        format!(
            r#"{{"name": "{name}", "browser_download_url": "https://example.invalid/{name}", "size": 7}}"#
        )
    }

    #[test]
    fn selects_the_asset_for_this_platform() {
        let assets = [
            asset("AugurRS-1.2.0-linux-x86_64.AppImage"),
            asset("AugurRS-1.2.0-macos-universal.dmg"),
            asset("AugurRS-1.2.0-windows-x86_64-setup.exe"),
            asset("SHA256SUMS"),
        ]
        .join(",");

        let release = payload(&assets, "").into_release(TARGET).unwrap();

        assert_eq!(release.version, Version::parse("1.2.0").unwrap());
        assert_eq!(release.asset.name, "AugurRS-1.2.0-linux-x86_64.AppImage");
        assert_eq!(release.asset.kind, PayloadKind::LinuxAppImage);
        assert!(release.checksums_url.ends_with("SHA256SUMS"));
    }

    #[test]
    fn refuses_a_release_without_checksums() {
        let assets = asset("AugurRS-1.2.0-linux-x86_64.AppImage");
        assert!(matches!(
            payload(&assets, "").into_release(TARGET),
            Err(UpdateError::MissingChecksums)
        ));
    }

    #[test]
    fn refuses_a_release_with_no_asset_for_this_platform() {
        let assets = [
            asset("AugurRS-1.2.0-macos-universal.dmg"),
            asset("SHA256SUMS"),
        ]
        .join(",");
        assert!(matches!(
            payload(&assets, "").into_release(TARGET),
            Err(UpdateError::NoAssetForPlatform { .. })
        ));
    }

    #[test]
    fn refuses_drafts_and_prereleases() {
        let assets = [
            asset("AugurRS-1.2.0-linux-x86_64.AppImage"),
            asset("SHA256SUMS"),
        ]
        .join(",");
        for extra in [r#""draft": true,"#, r#""prerelease": true,"#] {
            assert!(matches!(
                payload(&assets, extra).into_release(TARGET),
                Err(UpdateError::NoStableRelease)
            ));
        }
    }

    #[test]
    fn refuses_a_tag_that_is_not_a_version() {
        let assets = [
            asset("AugurRS-1.2.0-linux-x86_64.AppImage"),
            asset("SHA256SUMS"),
        ]
        .join(",");
        let mut release = payload(&assets, "");
        release.tag_name = "nightly".to_owned();
        assert!(matches!(
            release.into_release(TARGET),
            Err(UpdateError::UnreadableTag(_))
        ));
    }
}
