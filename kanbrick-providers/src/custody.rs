//! Per-employee provider-key custody (P9.3, #103) — the seam, namespacing, and an
//! in-memory backend.
//!
//! Provider keys are **retrievable** secrets (unlike `kanbrick-auth`'s hash-only
//! [`ApiKeyService`]), so per ADR-0009 they live in an at-rest enclave
//! **namespaced by the JWT-derived `FirmContext.user_id`**: every operation takes
//! the caller's `user_id` and a backend can only ever touch that user's namespace,
//! so a key is unreadable cross-user *by construction*. The webview never sees a
//! secret — only [`KeyMetadata`] crosses outward (it has no secret field), keeping
//! identity host-authoritative (ADR-0016).
//!
//! This module ships the **trait** ([`ProviderKeyStore`]) plus an
//! [`InMemoryKeyStore`] that the firm-OS workspace and CI compile and test with no
//! native dependencies. ADR-0009's primary backend — the IOTA Stronghold enclave —
//! carries a native `libsodium-sys` dependency that, per that ADR, enters the
//! **cockpit build, not this workspace**; the Stronghold and OS-keychain backends
//! therefore implement this same trait on the cockpit side (see
//! `docs/probes/p9.3-key-custody.md`). The `auth::Session` seam (ADR-0009 §3) holds
//! a `dyn ProviderKeyStore`, so callers are identical across backends.
//!
//! [`ApiKeyService`]: https://docs.rs/kanbrick-auth

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ProviderKind;

/// Identifier for one stored provider key, unique within a user's namespace.
///
/// A serde **newtype** over [`Uuid`], so it deserializes from a plain UUID string
/// (an axum `Path<KeyId>` extractor parses a route segment directly) and serializes
/// back to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyId(pub Uuid);

impl std::fmt::Display for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Non-secret metadata about a stored key.
///
/// This is the **only** shape that crosses to the webview or the audit log — it
/// has no secret field, so `GET /me/provider-keys` and every audit record are
/// metadata-only by construction, not by remembering to redact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMetadata {
    /// The key's identifier within the owning user's namespace.
    pub id: KeyId,
    /// Which provider the key authenticates to.
    pub provider: ProviderKind,
    /// A human label the employee gave the key (e.g. `"personal-openai"`).
    pub label: String,
    /// Creation time, Unix seconds.
    pub created_at: i64,
}

/// A failure from a custody backend, normalized across Stronghold / keychain /
/// in-memory implementations.
#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    /// The backend (enclave, keychain, lock) failed.
    #[error("provider key store backend error: {0}")]
    Backend(String),
}

/// Host-side custody of per-employee provider keys, namespaced by `user_id`.
///
/// Object-safe and `Send + Sync` so the app can hold `Arc<dyn ProviderKeyStore>`
/// and swap the Stronghold backend in on the cockpit side. **Every method takes the
/// caller's `user_id`**; an implementation must scope all access to that namespace
/// so no `(user_id, KeyId)` from one employee can read, list, or delete another's.
pub trait ProviderKeyStore: Send + Sync {
    /// Store `secret` under `user_id` for `provider`, returning its metadata. The
    /// backend assigns the [`KeyId`]; the secret is never returned by this call.
    fn put(
        &self,
        user_id: Uuid,
        provider: ProviderKind,
        label: &str,
        secret: &str,
    ) -> Result<KeyMetadata, KeyStoreError>;

    /// Retrieve the plaintext secret for `(user_id, id)` — **host-side only**, used
    /// to build an outbound provider call. Returns `None` if no such key exists in
    /// this user's namespace (including when `id` belongs to another user).
    fn get_secret(&self, user_id: Uuid, id: KeyId) -> Result<Option<String>, KeyStoreError>;

    /// List metadata for every key in `user_id`'s namespace (never the secrets).
    fn list(&self, user_id: Uuid) -> Result<Vec<KeyMetadata>, KeyStoreError>;

    /// Delete `(user_id, id)`. Returns `true` if a key was removed, `false` if it
    /// was absent from this user's namespace.
    fn delete(&self, user_id: Uuid, id: KeyId) -> Result<bool, KeyStoreError>;
}

/// A process-memory [`ProviderKeyStore`]: a per-user map of keys behind a `Mutex`.
///
/// This is the workspace/CI-testable backend and the reference for the cross-user
/// isolation invariant — the outer map is keyed on `user_id`, so every operation
/// is confined to one namespace. It is **not** at-rest encrypted; the durable
/// backends (Stronghold, OS keychain) live on the cockpit side per ADR-0009. It is
/// also the natural stand-in when `libsodium` is unavailable and no keychain is
/// configured (secrets live only for the process lifetime).
#[derive(Default)]
pub struct InMemoryKeyStore {
    inner: Mutex<Namespaces>,
}

struct StoredKey {
    metadata: KeyMetadata,
    secret: String,
}

/// The per-user keyring: each `user_id` maps to its own `KeyId -> StoredKey` map.
/// Aliased so the nested map stays under clippy's `type_complexity` bar where it
/// appears in the `Mutex` field and the `lock()` guard return.
type Namespaces = HashMap<Uuid, HashMap<KeyId, StoredKey>>;

impl InMemoryKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProviderKeyStore for InMemoryKeyStore {
    fn put(
        &self,
        user_id: Uuid,
        provider: ProviderKind,
        label: &str,
        secret: &str,
    ) -> Result<KeyMetadata, KeyStoreError> {
        let metadata = KeyMetadata {
            id: KeyId(Uuid::new_v4()),
            provider,
            label: label.to_string(),
            created_at: now_unix(),
        };
        let mut guard = self.lock()?;
        guard.entry(user_id).or_default().insert(
            metadata.id,
            StoredKey {
                metadata: metadata.clone(),
                secret: secret.to_string(),
            },
        );
        Ok(metadata)
    }

    fn get_secret(&self, user_id: Uuid, id: KeyId) -> Result<Option<String>, KeyStoreError> {
        let guard = self.lock()?;
        Ok(guard
            .get(&user_id)
            .and_then(|ns| ns.get(&id))
            .map(|stored| stored.secret.clone()))
    }

    fn list(&self, user_id: Uuid) -> Result<Vec<KeyMetadata>, KeyStoreError> {
        let guard = self.lock()?;
        Ok(guard
            .get(&user_id)
            .map(|ns| ns.values().map(|stored| stored.metadata.clone()).collect())
            .unwrap_or_default())
    }

    fn delete(&self, user_id: Uuid, id: KeyId) -> Result<bool, KeyStoreError> {
        let mut guard = self.lock()?;
        Ok(guard
            .get_mut(&user_id)
            .map(|ns| ns.remove(&id).is_some())
            .unwrap_or(false))
    }
}

impl InMemoryKeyStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Namespaces>, KeyStoreError> {
        self.inner
            .lock()
            .map_err(|_| KeyStoreError::Backend("in-memory store lock poisoned".to_string()))
    }
}

/// Current time in Unix seconds (saturating to `0`/`i64::MAX` rather than panicking
/// on a clock before the epoch or far in the future).
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn put_then_get_secret_round_trips() {
        let store = InMemoryKeyStore::new();
        let user = Uuid::new_v4();
        let meta = store
            .put(user, ProviderKind::OpenAI, "personal", "sk-secret")
            .unwrap();
        assert_eq!(meta.provider, ProviderKind::OpenAI);
        assert_eq!(meta.label, "personal");
        assert_eq!(
            store.get_secret(user, meta.id).unwrap().as_deref(),
            Some("sk-secret")
        );
    }

    #[test]
    fn keys_are_namespaced_cross_user_access_impossible() {
        let store = InMemoryKeyStore::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let meta = store
            .put(alice, ProviderKind::Anthropic, "a-key", "alice-secret")
            .unwrap();

        // Bob knows Alice's KeyId but is in a different namespace: every access misses.
        assert_eq!(store.get_secret(bob, meta.id).unwrap(), None);
        assert!(store.list(bob).unwrap().is_empty());
        assert!(!store.delete(bob, meta.id).unwrap());

        // Alice's own access is unaffected by Bob's attempts.
        assert_eq!(
            store.get_secret(alice, meta.id).unwrap().as_deref(),
            Some("alice-secret")
        );
        assert_eq!(store.list(alice).unwrap().len(), 1);
    }

    #[test]
    fn list_returns_metadata_only_never_the_secret() {
        let store = InMemoryKeyStore::new();
        let user = Uuid::new_v4();
        store
            .put(user, ProviderKind::Cerebras, "c-key", "super-secret-value")
            .unwrap();
        let listed = store.list(user).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider, ProviderKind::Cerebras);
        // The metadata shape has no secret field — serializing it cannot leak one.
        let json = serde_json::to_string(&listed[0]).unwrap();
        assert!(!json.contains("super-secret-value"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn delete_removes_and_is_idempotent() {
        let store = InMemoryKeyStore::new();
        let user = Uuid::new_v4();
        let meta = store.put(user, ProviderKind::OpenAI, "k", "s").unwrap();
        assert!(store.delete(user, meta.id).unwrap());
        assert_eq!(store.get_secret(user, meta.id).unwrap(), None);
        // Deleting again is a no-op, not an error.
        assert!(!store.delete(user, meta.id).unwrap());
    }

    #[test]
    fn is_object_safe_behind_arc() {
        let store: Arc<dyn ProviderKeyStore> = Arc::new(InMemoryKeyStore::new());
        let user = Uuid::new_v4();
        let meta = store.put(user, ProviderKind::OpenAI, "k", "s").unwrap();
        assert_eq!(
            store.get_secret(user, meta.id).unwrap().as_deref(),
            Some("s")
        );
    }

    #[test]
    fn key_id_serde_is_transparent_over_uuid() {
        let raw = Uuid::new_v4();
        let id = KeyId(raw);
        // Newtype serializes as the bare UUID string (so Path<KeyId> parses a segment).
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{raw}\""));
        let back: KeyId = serde_json::from_str(&format!("\"{raw}\"")).unwrap();
        assert_eq!(back, id);
    }
}
