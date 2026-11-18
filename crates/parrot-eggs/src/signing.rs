//! Signing a static registry: the key comes from an environment variable, never a command line.

pub use hatchery::sign::SigningKey;

/// Parses a signing key as `hatchery keygen` writes it: `ed25519:` and 64 hex digits.
pub fn parse(key: &str) -> Result<SigningKey, String> {
    hatchery::sign::parse_signing_key(key).map_err(|e| e.to_string())
}

/// The public key for `key`, `ed25519:` and 64 hex digits, which readers give to Hatchery as
/// `key=` so that an egg cannot be changed without it.
pub fn public_key(key: &SigningKey) -> String {
    hatchery::sign::format_verifying_key(&key.verifying_key())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_round_trips_and_says_its_public_half() {
        let text = "ed25519:0707070707070707070707070707070707070707070707070707070707070707";
        let key = parse(text).unwrap();
        let public = public_key(&key);
        assert!(public.starts_with("ed25519:"), "{public}");
        assert_eq!(public.len(), "ed25519:".len() + 64);
        assert!(parse("not a key").is_err());
    }
}
