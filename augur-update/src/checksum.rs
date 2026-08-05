//! SHA-256 verification against the release's `SHA256SUMS`.
//!
//! Verification is mandatory, not advisory. An update payload is an executable
//! that is about to replace the running program, so a missing or mismatched
//! digest aborts the update rather than warning about it.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Look up the expected digest for `asset_name` in a `sha256sum`-format file.
///
/// Accepts both the text (`hash  name`) and binary (`hash *name`) markers that
/// `sha256sum` and `shasum` produce.
pub fn expected_digest(sums: &str, asset_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*').trim();
        // Entries are written from a flat directory, but tolerate a path
        // prefix so a manually produced sums file still matches.
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        (name == asset_name && is_sha256_hex(digest)).then(|| digest.to_ascii_lowercase())
    })
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Streaming SHA-256 of a file, so a large payload never has to be held in
/// memory just to be checked.
pub fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SUMS: &str = concat!(
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  AugurRS-1.2.0-linux-x86_64.AppImage\n",
        "0000000000000000000000000000000000000000000000000000000000000001 *AugurRS-1.2.0-windows-x86_64-setup.exe\n",
        "0000000000000000000000000000000000000000000000000000000000000002  dist/AugurRS-1.2.0-macos-universal.dmg\n",
    );

    #[test]
    fn finds_entries_in_both_sha256sum_markers() {
        assert_eq!(
            expected_digest(SUMS, "AugurRS-1.2.0-linux-x86_64.AppImage").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            expected_digest(SUMS, "AugurRS-1.2.0-windows-x86_64-setup.exe").as_deref(),
            Some("0000000000000000000000000000000000000000000000000000000000000001")
        );
    }

    #[test]
    fn tolerates_a_path_prefix() {
        assert_eq!(
            expected_digest(SUMS, "AugurRS-1.2.0-macos-universal.dmg").as_deref(),
            Some("0000000000000000000000000000000000000000000000000000000000000002")
        );
    }

    #[test]
    fn missing_or_malformed_entries_yield_nothing() {
        // A name that is not listed must not fall through to some other digest.
        assert_eq!(
            expected_digest(SUMS, "AugurRS-9.9.9-linux-x86_64.AppImage"),
            None
        );
        assert_eq!(expected_digest("not a sums file", "anything"), None);
        assert_eq!(
            expected_digest("zz  short-hash-file", "short-hash-file"),
            None
        );
    }

    #[test]
    fn hashes_file_contents() {
        let mut path = std::env::temp_dir();
        path.push(format!("augur-update-digest-{}.bin", std::process::id()));
        File::create(&path).unwrap().write_all(b"augur").unwrap();

        assert_eq!(
            file_digest(&path).unwrap(),
            "40b5553ca09e063dd656c832e267f9914c07e2f6fdd22213588b4ad09aed198d"
        );

        // An empty file must hash to the well-known empty digest, which also
        // proves the streaming loop terminates correctly on a zero-byte read.
        let empty = path.with_extension("empty");
        File::create(&empty).unwrap();
        assert_eq!(
            file_digest(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&empty);
    }
}
