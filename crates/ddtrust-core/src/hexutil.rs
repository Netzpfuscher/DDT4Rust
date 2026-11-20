use crate::error::{Error, Result};

pub fn clean_hex(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Accepts "21 80", "2180", "0x21,0x80" and returns an even-length uppercase hex string.
pub fn normalize_hex(input: &str) -> Result<String> {
    let mut s = String::new();
    for token in input.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let token = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);
        if !token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Hex(format!("invalid hex token {token:?}")));
        }
        s.push_str(token);
    }
    if s.is_empty() {
        return Err(Error::Hex("empty hex string".into()));
    }
    if s.len() % 2 != 0 {
        return Err(Error::Hex(format!("odd number of hex digits in {input:?}")));
    }
    Ok(s.to_ascii_uppercase())
}

pub fn parse_hex(input: &str) -> Result<Vec<u8>> {
    let s = normalize_hex(input)?;
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| Error::Hex(e.to_string()))?);
    }
    Ok(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

pub fn to_hex_spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn hex_byte(s: &str) -> Result<u8> {
    u8::from_str_radix(s.trim(), 16).map_err(|e| Error::Hex(e.to_string()))
}

/// Format a hex string as space-separated byte pairs, ignoring any existing whitespace.
pub fn spaced_hex(input: &str) -> String {
    let c = clean_hex(input);
    c.as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_various() {
        assert_eq!(normalize_hex("21 80").unwrap(), "2180");
        assert_eq!(normalize_hex("0x21,0x80").unwrap(), "2180");
        assert!(normalize_hex("218").is_err());
    }
}
