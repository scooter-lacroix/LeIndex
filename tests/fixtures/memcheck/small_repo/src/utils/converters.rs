//! Type conversion utilities.

/// Convert a string to a boolean.
pub fn str_to_bool(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Convert a boolean to a string.
pub fn bool_to_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Try to parse a string as an integer.
pub fn parse_int(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

/// Try to parse a string as a float.
pub fn parse_float(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

/// Convert bytes to a hex string.
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Convert a hex string to bytes.
pub fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}
