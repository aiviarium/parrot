//! The runtime's static X25519 key, runtime ids, and pinned peer keys (spec 24.3, E9).
//!
//! The private key lives in `link.key` in the state directory with mode `0600`. It is never
//! printed: [`StaticKey`] has no `Display`, and its `Debug` shows only the public key.

use crate::base32;
use sha2::{Digest, Sha256};
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The name of the key file in the state directory.
pub const KEY_FILE: &str = "link.key";

const KEY_LEN: usize = 32;

/// An X25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(pub [u8; KEY_LEN]);

impl PublicKey {
    /// The key as 64 lower-case hex digits, the form `Link.peer.key` stores.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn from_hex(text: &str) -> Option<PublicKey> {
        let bytes = unhex(text.trim())?;
        Some(PublicKey(bytes.try_into().ok()?))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<PublicKey> {
        Some(PublicKey(bytes.try_into().ok()?))
    }

    /// The runtime id this key proves (E9).
    pub fn runtime_id(&self) -> String {
        runtime_id(self)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// `rt_` and the first 26 characters of the Crockford base32 SHA-256 of the public key (E9), so a
/// runtime can't claim an id it holds no key for.
pub fn runtime_id(key: &PublicKey) -> String {
    let digest = Sha256::digest(key.0);
    let mut id = String::from("rt_");
    id.push_str(&base32::encode(&digest)[..26]);
    id
}

/// The runtime's static key pair.
pub struct StaticKey {
    private: [u8; KEY_LEN],
    public: PublicKey,
}

impl StaticKey {
    /// A new random key from the OS generator.
    pub fn generate() -> Result<StaticKey, snow::Error> {
        let mut dh = dh();
        dh.generate(
            &mut *DefaultResolver
                .resolve_rng()
                .expect("the default resolver has an RNG"),
        )?;
        Ok(StaticKey::from_private(
            dh.privkey().try_into().expect("X25519 keys are 32 bytes"),
        ))
    }

    /// The key pair of a private key. Tests use fixed made-up private keys.
    pub fn from_private(private: [u8; KEY_LEN]) -> StaticKey {
        let mut dh = dh();
        dh.set(&private);
        let public = PublicKey(dh.pubkey().try_into().expect("X25519 keys are 32 bytes"));
        StaticKey { private, public }
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    pub fn runtime_id(&self) -> String {
        runtime_id(&self.public)
    }

    /// The private key, for the Noise handshake only.
    pub(crate) fn private(&self) -> &[u8; KEY_LEN] {
        &self.private
    }

    /// Loads `link.key` from `state_dir`, creating the directory (`0700`) and a new key (`0600`)
    /// when there is none.
    pub fn load_or_create(state_dir: &Path) -> io::Result<StaticKey> {
        let path = state_dir.join(KEY_FILE);
        match StaticKey::load(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            other => return other,
        }
        create_private_dir(state_dir)?;
        let key = StaticKey::generate().map_err(io::Error::other)?;
        // Written under a temporary name and hard-linked into place, so a reader never sees a
        // partial file and two runtimes starting together keep the same key.
        let tmp = state_dir.join(format!(".{KEY_FILE}.{:016x}", rand::random::<u64>()));
        let written = write_private(&tmp, &key).and_then(|()| std::fs::hard_link(&tmp, &path));
        let _ = std::fs::remove_file(&tmp);
        match written {
            Ok(()) => Ok(key),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => StaticKey::load(&path),
            Err(e) => Err(e),
        }
    }

    /// Loads a key file. Refuses a file that isn't a regular file owned by this user with no
    /// access for group or others.
    pub fn load(path: &Path) -> io::Result<StaticKey> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.file_type().is_file() {
            return Err(invalid(path, "is not a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if meta.uid() != rustix::process::geteuid().as_raw() {
                return Err(invalid(path, "is owned by another user"));
            }
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(invalid(
                    path,
                    "is readable by other users; it must have mode 0600",
                ));
            }
        }
        let text = std::fs::read_to_string(path)?;
        let bytes = unhex(text.trim()).ok_or_else(|| invalid(path, "does not hold a key"))?;
        let private: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| invalid(path, "does not hold a key"))?;
        Ok(StaticKey::from_private(private))
    }
}

impl fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl Drop for StaticKey {
    fn drop(&mut self) {
        for b in self.private.iter_mut() {
            // SAFETY: a valid, aligned pointer to a byte of this struct.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

fn dh() -> Box<dyn snow::types::Dh> {
    DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .expect("the default resolver has X25519")
}

fn invalid(path: &Path, what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{} {what}", path.display()))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn write_private(path: &PathBuf, key: &StaticKey) -> io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(path)?;
    let mut text = hex(&key.private);
    text.push('\n');
    let result = file.write_all(text.as_bytes()).and_then(|()| file.sync_all());
    // SAFETY: overwriting the bytes of a String we own; zeros are valid UTF-8.
    unsafe { text.as_bytes_mut().fill(0) };
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// The keys a runtime accepts outside pairing. The runtime backs this with its link records.
pub trait Pins: Send + Sync {
    /// Whether `key` is pinned for some link.
    fn is_pinned(&self, key: &PublicKey) -> bool;
}

/// Pinned keys in memory, by runtime id.
#[derive(Default)]
pub struct PinSet {
    keys: RwLock<HashMap<String, PublicKey>>,
}

impl PinSet {
    pub fn new() -> PinSet {
        PinSet::default()
    }

    /// Pins `key` and returns its runtime id.
    pub fn pin(&self, key: PublicKey) -> String {
        let id = key.runtime_id();
        self.keys.write().expect("pins lock").insert(id.clone(), key);
        id
    }

    pub fn unpin(&self, runtime: &str) -> Option<PublicKey> {
        self.keys.write().expect("pins lock").remove(runtime)
    }

    pub fn get(&self, runtime: &str) -> Option<PublicKey> {
        self.keys.read().expect("pins lock").get(runtime).copied()
    }
}

impl Pins for PinSet {
    fn is_pinned(&self, key: &PublicKey) -> bool {
        self.get(&key.runtime_id()) == Some(*key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let key = StaticKey::from_private([7; 32]).public();
        assert_eq!(PublicKey::from_hex(&key.to_hex()), Some(key));
        assert_eq!(PublicKey::from_hex("abc"), None);
        assert_eq!(PublicKey::from_hex(&"zz".repeat(32)), None);
    }

    #[test]
    fn debug_hides_the_private_key() {
        let key = StaticKey::from_private([0x42; 32]);
        let shown = format!("{key:?}");
        assert!(!shown.contains(&"42".repeat(32)), "{shown}");
        assert!(shown.contains(&key.public().to_hex()));
    }
}
