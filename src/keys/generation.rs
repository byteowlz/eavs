//! Key generation utilities.

use rand::Rng;
use sha2::{Digest, Sha256};

use super::word_lists::WordLists;

/// Key prefix for EAVS virtual keys.
pub const KEY_PREFIX: &str = "eavs-";

/// Base62 alphabet for key encoding.
const BASE62_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Generate a new virtual API key.
///
/// Format: `eavs-{32 base62 characters}`
/// Example: `eavs-7k9Xp2mN4qR8vT3wY6zA1bC5dE0fG1hI`
///
/// The key contains 192 bits of entropy (24 random bytes encoded as base62).
pub fn generate_key() -> String {
    let mut rng = rand::thread_rng();
    let random_bytes: [u8; 24] = rng.gen();

    let encoded = base62_encode(&random_bytes);
    format!("{}{}", KEY_PREFIX, encoded)
}

/// Generate a human-readable key ID using word lists.
///
/// Format: `adjective-noun` (e.g., "cold-lamp", "blue-frog")
///
/// Uses embedded word lists for generation. The combination provides
/// ~40,000 unique IDs (200 adjectives * 200 nouns).
pub fn generate_human_id() -> String {
    let words = WordLists::embedded();
    words.generate_id().unwrap_or_else(|| {
        // Fallback to short random ID if word generation fails
        let mut rng = rand::thread_rng();
        let bytes: [u8; 4] = rng.gen();
        format!("key-{}", base62_encode(&bytes))
    })
}

/// Generate a hash of a key for storage.
///
/// We store the hash rather than the key itself for security.
pub fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Verify that a key matches its stored hash.
#[allow(dead_code)]
pub fn verify_key_hash(key: &str, stored_hash: &str) -> bool {
    let computed_hash = hash_key(key);
    // Use constant-time comparison to prevent timing attacks
    constant_time_eq(computed_hash.as_bytes(), stored_hash.as_bytes())
}

/// Check if a string looks like an EAVS virtual key.
pub fn is_virtual_key(key: &str) -> bool {
    key.starts_with(KEY_PREFIX) && key.len() >= 36
}

/// Extract the key ID (first 8 chars after prefix) for logging.
#[allow(dead_code)]
pub fn key_id_prefix(key: &str) -> &str {
    if key.len() > 13 {
        &key[..13] // "eavs-" + 8 chars
    } else {
        key
    }
}

/// Encode bytes as base62.
fn base62_encode(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    let mut carry = 0u16;
    let mut carry_bits = 0u8;

    for &byte in bytes {
        carry = (carry << 8) | byte as u16;
        carry_bits += 8;

        while carry_bits >= 6 {
            carry_bits -= 6;
            let index = ((carry >> carry_bits) & 0x3F) as usize;
            // Map 6-bit value to base62 (0-61)
            let char_index = if index < 62 { index } else { index % 62 };
            result.push(BASE62_ALPHABET[char_index] as char);
        }
    }

    // Handle remaining bits
    if carry_bits > 0 {
        let index = ((carry << (6 - carry_bits)) & 0x3F) as usize;
        let char_index = if index < 62 { index } else { index % 62 };
        result.push(BASE62_ALPHABET[char_index] as char);
    }

    result
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// Inline hex encoding to avoid adding a dependency just for this
mod hex {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        let mut result = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            result.push(HEX_CHARS[(b >> 4) as usize] as char);
            result.push(HEX_CHARS[(b & 0xf) as usize] as char);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_key_format() {
        let key = generate_key();

        assert!(key.starts_with(KEY_PREFIX));
        assert!(key.len() >= 36); // "eavs-" + at least 31 chars
        assert!(is_virtual_key(&key));
    }

    #[test]
    fn test_generate_key_uniqueness() {
        let key1 = generate_key();
        let key2 = generate_key();

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_hash_key() {
        let key = "eavs-test123456789";
        let hash = hash_key(key);

        // SHA-256 produces 64 hex characters
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Same key should produce same hash
        assert_eq!(hash, hash_key(key));
    }

    #[test]
    fn test_verify_key_hash() {
        let key = generate_key();
        let hash = hash_key(&key);

        assert!(verify_key_hash(&key, &hash));
        assert!(!verify_key_hash("wrong-key", &hash));
    }

    #[test]
    fn test_is_virtual_key() {
        assert!(is_virtual_key("eavs-abc123456789012345678901234567890"));
        assert!(!is_virtual_key("sk-abc123")); // OpenAI key
        assert!(!is_virtual_key("eavs-short")); // Too short
    }

    #[test]
    fn test_key_id_prefix() {
        let key = "eavs-abc123456789012345678901234567890";
        assert_eq!(key_id_prefix(key), "eavs-abc12345");
    }

    #[test]
    fn test_base62_encode() {
        // Test that encoding produces only valid base62 characters
        let bytes = [0u8, 255, 128, 64, 32, 16, 8, 4, 2, 1];
        let encoded = base62_encode(&bytes);

        for c in encoded.chars() {
            assert!(
                c.is_ascii_alphanumeric(),
                "Invalid character in base62 encoding: {}",
                c
            );
        }
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
    }
}
