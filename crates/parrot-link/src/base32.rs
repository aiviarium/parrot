//! Crockford base32, the alphabet of Parrot ids (spec 2).

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encodes `bytes` most significant bit first, padding the last character with zero bits.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut acc: u16 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | b as u16;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes what [`encode`] wrote. Lower case is accepted, and `O`, `I` and `L` read as `0`, `1`
/// and `1`. Fails on any other character or on non-zero padding bits.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut acc: u16 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c.to_ascii_uppercase() {
            b'O' => 0,
            b'I' | b'L' => 1,
            u => ALPHABET.iter().position(|&a| a == u)? as u16,
        };
        acc = ((acc << 5) | v) & 0x1fff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if acc & ((1 << bits) - 1) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(encode(&[0xff]), "ZW");
        assert_eq!(encode(b"f"), "CR");
        assert_eq!(decode("cr").unwrap(), b"f");
        assert!(decode("CS").is_none(), "padding bits must be zero");
        assert!(decode("U0").is_none());
    }
}
