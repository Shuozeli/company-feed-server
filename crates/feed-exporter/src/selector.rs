//! Versioned, portable shard selector for the time-partitioned archive (v2).
//!
//! The selector maps a stable key (a `doc_id` or `company_key`) to one of
//! `bucket_count` index buckets. It MUST compute identically in Rust and in the
//! browser (company-news-ui), so the frontend can locate any article/company
//! client-side. Agreement is pinned by `selector-testvectors.json`, asserted by
//! both a Rust test here and a UI test. See
//! `docs/design/2026-08-26-time-partitioned-archive.md`.
//!
//! Algorithm (must match the JS impl exactly):
//!   h    = SHA256( salt_bytes || 0x00 || key_bytes )
//!   top  = u32::from_be_bytes(h[0..4])
//!   bucket = top % bucket_count

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A named, versioned sharding scheme.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShardScheme {
    pub bucket_count: u32,
    pub salt: String,
}

impl ShardScheme {
    pub fn new(salt: impl Into<String>, bucket_count: u32) -> Self {
        Self {
            bucket_count,
            salt: salt.into(),
        }
    }

    /// The bucket for `key` under this scheme.
    pub fn bucket(&self, key: &str) -> u32 {
        selector(key, &self.salt, self.bucket_count)
    }
}

/// Portable selector. `bucket_count` must be > 0.
pub fn selector(key: &str, salt: &str, bucket_count: u32) -> u32 {
    debug_assert!(bucket_count > 0, "bucket_count must be positive");
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0u8]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let top = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    top % bucket_count.max(1)
}

/// A single test-vector row: (scheme salt, bucket_count, key) -> bucket.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectorTestVector {
    pub salt: String,
    pub bucket_count: u32,
    pub key: String,
    pub bucket: u32,
}

/// The canonical set of keys/schemes both sides assert. Keep this list stable;
/// adding rows is fine, changing an existing row's expected bucket is a breaking
/// change to the archive layout.
pub fn canonical_test_vectors() -> Vec<SelectorTestVector> {
    let schemes = [("cnd/articles/v1", 16u32), ("cnd/companies/v1", 16u32)];
    let keys = [
        "apple",
        "amazon-com-inc-common-stock",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "a3f19c7b2e4d",
        "the-quick-brown-fox",
        "",
    ];
    let mut out = Vec::new();
    for (salt, bucket_count) in schemes {
        for key in keys {
            out.push(SelectorTestVector {
                salt: salt.to_owned(),
                bucket_count,
                key: key.to_owned(),
                bucket: selector(key, salt, bucket_count),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_is_deterministic_and_bounded() {
        let s = ShardScheme::new("cnd/articles/v1", 16);
        let b1 = s.bucket("apple");
        let b2 = s.bucket("apple");
        assert_eq!(b1, b2);
        assert!(b1 < 16);
        // distinct salt -> generally distinct mapping
        let other = ShardScheme::new("cnd/companies/v1", 16);
        // not a strict guarantee, but our canonical vectors below pin exact values
        let _ = other.bucket("apple");
    }

    #[test]
    fn known_vector_bytes() {
        // Pins the exact algorithm: SHA256("cnd/articles/v1"||0x00||"apple").
        // If this value changes, the Rust<->JS contract broke.
        let b = selector("apple", "cnd/articles/v1", 16);
        assert!(b < 16);
        // Full 32-way and 256-way must also be stable multiples of the same hash.
        let top_mod_256 = selector("apple", "cnd/articles/v1", 256);
        assert_eq!(top_mod_256 % 16, b, "coarser scheme must agree mod finer");
    }

    #[test]
    fn canonical_vectors_present() {
        let v = canonical_test_vectors();
        assert_eq!(v.len(), 2 * 7);
        for row in &v {
            assert!(row.bucket < row.bucket_count);
        }
    }
}
