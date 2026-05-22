//! Cryptographic utility functions (simplified for fixture).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Compute a simple hash of a string.
pub fn simple_hash(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Generate a simple token.
pub fn generate_token(user_id: u64, timestamp: u64) -> String {
    let raw = format!("{}:{}", user_id, timestamp);
    format!("{:016x}", simple_hash(&raw))
}

/// Verify a token matches expected user and timestamp.
pub fn verify_token(token: &str, user_id: u64, timestamp: u64) -> bool {
    let expected = generate_token(user_id, timestamp);
    token == expected
}

/// Obfuscate a string for logging.
pub fn obfuscate(s: &str) -> String {
    if s.len() <= 4 {
        "*".repeat(s.len())
    } else {
        format!("{}{}{}", &s[..2], "*".repeat(s.len() - 4), &s[s.len() - 2..])
    }
}
