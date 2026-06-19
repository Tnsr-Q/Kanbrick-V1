//! Content-addressed, air-gapped guest asset store (#64, Track C).
//!
//! Guest WASM artifacts are stored on a local volume keyed by the SHA-256 of
//! their bytes, under `<root>/sha256/<hex>.wasm`. A stored artifact is named by a
//! canonical URI `tachyon://sha256:<hex>` (the scheme aligns cosmetically with the
//! upstream Tachyon registry; Kanbrick does not use the Tachyon host). The digest
//! is verified on every write **and** on every read, so a corrupted or swapped
//! file is caught before it is ever compiled.
//!
//! This module is pure filesystem + hashing — it owns no graph state. The bytes
//! live here; the *policy* that binds a guest name to an asset URI lives in
//! SparrowDB (`kanbrick_store::guest_policy`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

/// The URI scheme + algorithm prefix for stored assets.
const URI_PREFIX: &str = "tachyon://sha256:";
/// Length of a hex-encoded SHA-256 digest.
const SHA256_HEX_LEN: usize = 64;
/// Disambiguates concurrent temp files written by this process.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A failure in the asset store, kept distinct so the HTTP layer can map each
/// case to the right status code.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// An empty artifact was offered for storage.
    #[error("asset body is empty")]
    Empty,
    /// The caller's expected digest did not match the bytes provided.
    #[error("sha256 mismatch: expected {expected}, computed {actual}")]
    HashMismatch {
        /// The digest the caller claimed.
        expected: String,
        /// The digest actually computed over the bytes.
        actual: String,
    },
    /// The asset URI was not a well-formed `tachyon://sha256:<hex>`.
    #[error("invalid asset uri {0:?}")]
    InvalidUri(String),
    /// No artifact is stored under the requested URI.
    #[error("asset {0} not found")]
    NotFound(String),
    /// A stored artifact's bytes no longer match its content address.
    #[error("asset {uri} failed its integrity check (stored bytes hash to {actual})")]
    Corrupt {
        /// The URI whose bytes were read.
        uri: String,
        /// The digest the bytes actually hash to now.
        actual: String,
    },
    /// An underlying filesystem error.
    #[error("asset i/o error: {0}")]
    Io(String),
}

/// A stored artifact: its canonical URI and hex digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    /// Canonical `tachyon://sha256:<hex>` URI.
    pub uri: String,
    /// Hex-encoded SHA-256 of the bytes.
    pub sha256: String,
}

/// A filesystem-backed, content-addressed store for guest WASM artifacts.
#[derive(Debug, Clone)]
pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    /// Create a store rooted at `root`. The directory is created lazily on the
    /// first [`put`](Self::put), so constructing a store never touches the disk
    /// (boot with no registry artifacts stays filesystem-free).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        AssetStore { root: root.into() }
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store `bytes`, returning their canonical [`AssetRef`]. If `expected_sha` is
    /// given, the bytes must hash to it (case-insensitive) or the write is
    /// rejected. The write is atomic: bytes land in a temp file that is renamed
    /// into place, so a reader never observes a partial artifact. Storing the same
    /// bytes again is idempotent (same content address).
    pub fn put(&self, bytes: &[u8], expected_sha: Option<&str>) -> Result<AssetRef, AssetError> {
        if bytes.is_empty() {
            return Err(AssetError::Empty);
        }
        let actual = sha256_hex(bytes);
        if let Some(expected) = expected_sha {
            let expected = expected.trim().to_ascii_lowercase();
            if expected != actual {
                return Err(AssetError::HashMismatch { expected, actual });
            }
        }

        let dir = self.root.join("sha256");
        std::fs::create_dir_all(&dir).map_err(io)?;
        let final_path = dir.join(format!("{actual}.wasm"));
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{actual}.{}.{seq}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes).map_err(io)?;
        // Rename is atomic within a filesystem; clean up the temp file on failure.
        if let Err(e) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io(e));
        }

        tracing::debug!(target: "kanbrick_mesh::assets", sha256 = %actual, "stored asset");
        Ok(AssetRef {
            uri: uri_for(&actual),
            sha256: actual,
        })
    }

    /// Read the artifact named by `uri`, re-verifying its digest before returning
    /// it. A missing file is [`AssetError::NotFound`]; bytes that no longer match
    /// the address are [`AssetError::Corrupt`].
    pub fn get(&self, uri: &str) -> Result<Vec<u8>, AssetError> {
        let hex = parse_uri(uri)?;
        let path = self.root.join("sha256").join(format!("{hex}.wasm"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AssetError::NotFound(uri.to_string()));
            }
            Err(e) => return Err(io(e)),
        };
        let actual = sha256_hex(&bytes);
        if actual != hex {
            return Err(AssetError::Corrupt {
                uri: uri.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// Whether an artifact is stored under `uri` (does not re-verify its digest).
    pub fn contains(&self, uri: &str) -> bool {
        match parse_uri(uri) {
            Ok(hex) => self
                .root
                .join("sha256")
                .join(format!("{hex}.wasm"))
                .is_file(),
            Err(_) => false,
        }
    }
}

/// Hex-encoded SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(SHA256_HEX_LEN);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The canonical URI for a hex digest.
fn uri_for(hex: &str) -> String {
    format!("{URI_PREFIX}{hex}")
}

/// Extract and validate the hex digest from a `tachyon://sha256:<hex>` URI.
fn parse_uri(uri: &str) -> Result<String, AssetError> {
    let hex = uri
        .strip_prefix(URI_PREFIX)
        .ok_or_else(|| AssetError::InvalidUri(uri.to_string()))?;
    if hex.len() == SHA256_HEX_LEN && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(hex.to_ascii_lowercase())
    } else {
        Err(AssetError::InvalidUri(uri.to_string()))
    }
}

/// Map a filesystem error into [`AssetError::Io`].
fn io(e: std::io::Error) -> AssetError {
    AssetError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AssetStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_d, store) = store();
        let bytes = b"\0asm fake guest payload".to_vec();
        let asset = store.put(&bytes, None).unwrap();
        assert!(asset.uri.starts_with(URI_PREFIX));
        assert_eq!(asset.sha256.len(), SHA256_HEX_LEN);
        assert_eq!(store.get(&asset.uri).unwrap(), bytes);
        assert!(store.contains(&asset.uri));
    }

    #[test]
    fn put_is_content_addressed_and_idempotent() {
        let (_d, store) = store();
        let a = store.put(b"same bytes", None).unwrap();
        let b = store.put(b"same bytes", None).unwrap();
        assert_eq!(a, b, "identical bytes get the same address");
    }

    #[test]
    fn empty_body_is_rejected() {
        let (_d, store) = store();
        assert!(matches!(store.put(b"", None), Err(AssetError::Empty)));
    }

    #[test]
    fn matching_expected_hash_is_accepted_mismatch_rejected() {
        let (_d, store) = store();
        let bytes = b"verify me".to_vec();
        let good = sha256_hex(&bytes);
        assert!(store.put(&bytes, Some(&good)).is_ok());
        assert!(store.put(&bytes, Some(&good.to_uppercase())).is_ok());
        match store.put(&bytes, Some("deadbeef")) {
            Err(AssetError::HashMismatch { actual, .. }) => assert_eq!(actual, good),
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn get_rejects_bad_uris_and_missing_assets() {
        let (_d, store) = store();
        assert!(matches!(
            store.get("http://example.com/x"),
            Err(AssetError::InvalidUri(_))
        ));
        assert!(matches!(
            store.get("tachyon://sha256:short"),
            Err(AssetError::InvalidUri(_))
        ));
        let absent = uri_for(&sha256_hex(b"never stored"));
        assert!(matches!(store.get(&absent), Err(AssetError::NotFound(_))));
    }

    #[test]
    fn get_detects_corruption() {
        let (_d, store) = store();
        let asset = store.put(b"original", None).unwrap();
        // Tamper with the stored file behind the store's back.
        let path = store
            .root()
            .join("sha256")
            .join(format!("{}.wasm", asset.sha256));
        std::fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            store.get(&asset.uri),
            Err(AssetError::Corrupt { .. })
        ));
    }
}
