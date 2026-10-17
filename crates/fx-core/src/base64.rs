//! Strict standard-alphabet base64 (RFC 4648 §4) with mandatory padding.
//!
//! Used only for image bytes at the JSON boundary. Rejects whitespace, URL-safe
//! characters, missing or misplaced padding and non-zero trailing bits, so every
//! byte string has exactly one accepted encoding.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Why base64 text was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base64Error {
    /// The text is not canonical padded base64.
    Invalid,
    /// The decoded length would exceed the caller's bound.
    TooLarge,
}

/// Number of bytes `text` decodes to, assuming it is well formed.
pub fn decoded_len(text: &str) -> usize {
    let padding = text.bytes().rev().take_while(|&b| b == b'=').count().min(2);
    ((text.len() / 4) * 3).saturating_sub(padding)
}

/// Encodes bytes.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes canonical padded base64, refusing output longer than `max_len` before
/// allocating it.
pub fn decode(text: &str, max_len: usize) -> Result<Vec<u8>, Base64Error> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(Base64Error::Invalid);
    }
    if decoded_len(text) > max_len {
        return Err(Base64Error::TooLarge);
    }
    let mut out = Vec::with_capacity(decoded_len(text));
    let quads = bytes.len() / 4;
    for (index, quad) in bytes.chunks(4).enumerate() {
        let last = index + 1 == quads;
        let pad = quad.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err(Base64Error::Invalid);
        }
        let mut n = 0u32;
        for &byte in &quad[..4 - pad] {
            n = (n << 6) | value(byte).ok_or(Base64Error::Invalid)?;
        }
        n <<= 6 * pad as u32;
        let produced = 3 - pad;
        // Canonical form: discarded bits must be zero.
        if (pad == 1 && n & 0xff != 0) || (pad == 2 && n & 0xffff != 0) {
            return Err(Base64Error::Invalid);
        }
        let triple = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&triple[..produced]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_every_length_up_to_64_bytes() {
        for len in 0..=64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let text = encode(&bytes);
            assert_eq!(decoded_len(&text), len);
            assert_eq!(decode(&text, len).unwrap(), bytes);
            if len > 0 {
                assert_eq!(decode(&text, len - 1), Err(Base64Error::TooLarge));
            }
        }
    }

    #[test]
    fn rejects_non_canonical_forms() {
        for bad in [
            "A", "AA", "AAA", "AB==", "AAB=", "A===", "=AAA", "AA=A", "AA==AA==", "AA\nA",
        ] {
            assert_eq!(decode(bad, 64), Err(Base64Error::Invalid), "{bad:?}");
        }
        assert_eq!(decode("AA==", 64).unwrap(), vec![0]);
        assert_eq!(decode("", 0).unwrap(), Vec::<u8>::new());
    }
}
