//! Persistent named camera/global configuration profiles.
//!
//! Profiles are host-owned. Runtime plugins may reference a name through the
//! host command contract, but they never read or write this directory.

#[cfg(not(windows))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use augur_core::config::CameraConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const PROFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CameraProfileV1 {
    pub schema_version: u32,
    pub name: String,
    pub revision: u64,
    pub camera: CameraConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileIdentity {
    pub name: String,
    pub schema_version: u32,
    pub revision: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedProfile {
    pub profile: CameraProfileV1,
    pub identity: ProfileIdentity,
}

#[derive(Debug, Clone)]
pub(crate) struct ProfileListEntry {
    pub name: String,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    pub(crate) fn in_app_config_dir() -> Result<Self, String> {
        let root = dirs::config_dir()
            .ok_or_else(|| {
                "the operating system did not provide a configuration directory".to_owned()
            })?
            .join("augur")
            .join("camera-profiles");
        Ok(Self { root })
    }

    #[cfg(test)]
    fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn list(&self) -> Result<Vec<ProfileListEntry>, String> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.root).map_err(|error| {
            format!(
                "cannot list profile directory {}: {error}",
                self.root.display()
            )
        })? {
            let path = item
                .map_err(|error| format!("cannot read profile entry: {error}"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let loaded = self.load_path(&path)?;
            entries.push(ProfileListEntry {
                name: loaded.profile.name,
                revision: loaded.profile.revision,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    pub(crate) fn load(&self, name: &str) -> Result<LoadedProfile, String> {
        let path = self.profile_path(name)?;
        self.load_path(&path)
    }

    pub(crate) fn save(
        &self,
        name: &str,
        camera: &CameraConfig,
        overwrite: bool,
    ) -> Result<LoadedProfile, String> {
        validate_name(name)?;
        camera
            .validate(camera.global.sensor_width, camera.global.sensor_height)
            .map_err(|error| format!("profile camera configuration is invalid: {error}"))?;
        fs::create_dir_all(&self.root).map_err(|error| {
            format!(
                "cannot create profile directory {}: {error}",
                self.root.display()
            )
        })?;
        let path = self.profile_path(name)?;
        let revision = if path.exists() {
            if !overwrite {
                return Err(format!("profile '{name}' already exists"));
            }
            self.load_path(&path)?.profile.revision.saturating_add(1)
        } else {
            1
        };
        let profile = CameraProfileV1 {
            schema_version: PROFILE_SCHEMA_VERSION,
            name: name.to_owned(),
            revision,
            camera: camera.clone(),
        };
        let encoded = toml::to_string_pretty(&profile)
            .map_err(|error| format!("cannot encode profile '{name}': {error}"))?;
        atomic_write(&path, encoded.as_bytes())?;
        self.load_path(&path)
    }

    pub(crate) fn delete(&self, name: &str) -> Result<(), String> {
        let path = self.profile_path(name)?;
        fs::remove_file(&path)
            .map_err(|error| format!("cannot delete profile '{}': {error}", path.display()))
    }

    fn profile_path(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        Ok(self.root.join(format!("{name}.toml")))
    }

    fn load_path(&self, path: &Path) -> Result<LoadedProfile, String> {
        let raw = fs::read(path)
            .map_err(|error| format!("cannot read profile '{}': {error}", path.display()))?;
        let profile: CameraProfileV1 = toml::from_str(
            std::str::from_utf8(&raw)
                .map_err(|error| format!("profile '{}' is not UTF-8: {error}", path.display()))?,
        )
        .map_err(|error| format!("profile '{}' is invalid TOML: {error}", path.display()))?;
        if profile.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(format!(
                "profile '{}' uses unsupported schema version {} (supported: {})",
                path.display(),
                profile.schema_version,
                PROFILE_SCHEMA_VERSION
            ));
        }
        validate_name(&profile.name)?;
        let expected_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if expected_stem != profile.name {
            return Err(format!(
                "profile '{}' contains name '{}' instead of '{}'",
                path.display(),
                profile.name,
                expected_stem
            ));
        }
        profile
            .camera
            .validate(
                profile.camera.global.sensor_width,
                profile.camera.global.sensor_height,
            )
            .map_err(|error| format!("profile '{}' is invalid: {error}", path.display()))?;
        let sha256 = format!("{:x}", Sha256::digest(&raw));
        let identity = ProfileIdentity {
            name: profile.name.clone(),
            schema_version: profile.schema_version,
            revision: profile.revision,
            sha256,
        };
        Ok(LoadedProfile { profile, identity })
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.trim() == name
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(
            "profile name must be 1-64 ASCII letters, digits, spaces, '-' or '_', without leading or trailing spaces"
                .to_owned(),
        )
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .ok_or_else(|| format!("profile path '{}' has no parent", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("profile");
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "cannot create temporary profile '{}': {error}",
                temporary.display()
            )
        })?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        // Windows does not allow a rename while this ordinary file handle is
        // open, even when the destination does not exist.
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "cannot atomically replace profile '{}': {error}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: Both pointers refer to live, null-terminated UTF-16 buffers for
    // the complete call. The function does not retain either pointer. Source
    // and destination are in the same profile directory, so this remains a
    // same-volume atomic replacement; WRITE_THROUGH waits for the move to be
    // flushed before reporting success.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    // `MOVEFILE_WRITE_THROUGH` above supplies the Windows durability barrier.
    // Opening a directory through `File::open` is not supported on Windows.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store(label: &str) -> ProfileStore {
        static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        ProfileStore::at(std::env::temp_dir().join(format!(
            "augur-profile-test-{}-{label}-{unique}-{sequence}",
            std::process::id()
        )))
    }

    #[test]
    fn profiles_round_trip_list_overwrite_and_delete() {
        let store = temp_store("crud");
        let mut camera = CameraConfig::default();
        camera.biases.diff_on = 12;
        camera.global.record_sensor_telemetry = true;
        let first = store.save("bench_a", &camera, false).expect("save");
        assert_eq!(first.identity.revision, 1);
        assert!(first.profile.camera.global.record_sensor_telemetry);
        assert_eq!(store.list().expect("list")[0].name, "bench_a");

        camera.biases.diff_on = 13;
        let second = store.save("bench_a", &camera, true).expect("overwrite");
        assert_eq!(second.identity.revision, 2);
        assert_ne!(second.identity.sha256, first.identity.sha256);
        assert_eq!(
            store
                .load("bench_a")
                .expect("load")
                .profile
                .camera
                .biases
                .diff_on,
            13
        );

        store.delete("bench_a").expect("delete");
        assert!(store.list().expect("empty list").is_empty());
        let _ = fs::remove_dir_all(store.root);
    }

    #[test]
    fn profile_names_cannot_escape_the_store() {
        let store = temp_store("traversal");
        for name in ["../outside", "a/b", "", " white space", "white space "] {
            assert!(store.save(name, &CameraConfig::default(), false).is_err());
        }
        assert!(store
            .save("low noise", &CameraConfig::default(), false)
            .is_ok());
        let _ = fs::remove_dir_all(store.root);
    }

    #[test]
    fn unknown_schema_and_corrupt_toml_fail_closed() {
        let store = temp_store("invalid");
        fs::create_dir_all(&store.root).expect("directory");
        fs::write(store.root.join("bad.toml"), "not = [toml").expect("corrupt profile");
        assert!(store
            .load("bad")
            .expect_err("corrupt must fail")
            .contains("invalid TOML"));
        let mut encoded = toml::to_string_pretty(&CameraProfileV1 {
            schema_version: 99,
            name: "future".into(),
            revision: 1,
            camera: CameraConfig::default(),
        })
        .expect("encode");
        encoded.push('\n');
        fs::write(store.root.join("future.toml"), encoded).expect("future profile");
        assert!(store
            .load("future")
            .expect_err("future schema must fail")
            .contains("unsupported schema"));
        let _ = fs::remove_dir_all(store.root);
    }
}
