//! Encryption at rest of the Immich API key.
//!
//! Only the API key is stored. The Immich password is used once to create the key and is never
//! written anywhere.
//!
//! # Scheme
//!
//! XChaCha20-Poly1305 with a 32-byte random key kept in `<data_dir>/immich.key` (raw bytes, created
//! with mode `0600`, never logged) and a fresh random 24-byte nonce per encryption. The associated
//! data is a constant purpose string, so a value encrypted for another purpose with the same key
//! does not decrypt here. The stored text is `v1.<base64url(nonce || ciphertext)>`; the version
//! prefix leaves room to change the scheme later.
//!
//! # Threat model
//!
//! This protects the database file when it is copied or shared on its own (a backup of the
//! database, a file attached to a bug report): without `immich.key` the API key in it cannot be
//! read. It does **not** protect against someone who can read the whole data directory, because
//! the key file lives next to the database, and it does not protect a running process. The
//! password-based alternative would need the user to type a passphrase at every start, which does
//! not fit an unattended NAS container.
//!
//! # Losing the key file
//!
//! A missing, truncated or foreign key file makes the stored value unreadable. That is not a crash:
//! [`SecretBox::load`] fails, the service reports the connection as needing a reconnect and the
//! next `connect` writes a new key file ([`SecretBox::load_or_create`]).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use thiserror::Error;

use crate::security::Redacted;

/// File name of the key inside the data directory.
pub const KEY_FILE_NAME: &str = "immich.key";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const VERSION_PREFIX: &str = "v1.";
/// Associated data of every stored key. It keeps the app's former name on purpose: changing it
/// makes every stored Immich API key undecryptable.
const PURPOSE: &[u8] = b"advanced-photo-manager/immich-api-key/v1";

#[derive(Debug, Error)]
pub enum CryptoError {
    /// There is no key file, so nothing stored can be read.
    #[error("the key file is missing")]
    MissingKey,
    /// The key file exists but does not hold a key.
    #[error("the key file is not a valid key")]
    BadKey,
    #[error("the key file cannot be accessed: {0}")]
    Io(#[from] io::Error),
    /// The stored text is not in the `v1.` format.
    #[error("the stored secret is malformed")]
    Malformed,
    /// Authentication failed: the key does not match, or the value was modified.
    #[error("the stored secret cannot be decrypted")]
    Decrypt,
}

/// Encrypts and decrypts short secrets with the key of one data directory.
pub struct SecretBox {
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBox").finish_non_exhaustive()
    }
}

/// Where the key of `data_dir` lives.
pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(KEY_FILE_NAME)
}

impl SecretBox {
    /// Opens the key of `data_dir`, creating one if there is none (or if the file cannot hold a
    /// key because it was truncated). Used when a new secret is about to be stored.
    pub fn load_or_create(data_dir: &Path) -> Result<Self, CryptoError> {
        let path = key_path(data_dir);
        match read_key(&path) {
            Ok(key) => Ok(Self::new(key)),
            Err(CryptoError::MissingKey) => Self::create(&path),
            Err(CryptoError::BadKey) => {
                tracing::warn!("the Immich key file is not a valid key; replacing it");
                fs::remove_file(&path)?;
                Self::create(&path)
            }
            Err(err) => Err(err),
        }
    }

    /// Opens the existing key of `data_dir`; never creates one. Used to read a stored secret.
    pub fn load(data_dir: &Path) -> Result<Self, CryptoError> {
        read_key(&key_path(data_dir)).map(Self::new)
    }

    fn create(path: &Path) -> Result<Self, CryptoError> {
        let mut key = [0u8; KEY_LEN];
        rand::fill(&mut key);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(path) {
            Ok(file) => file,
            // Someone else created it between our check and now: use theirs.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                return read_key(path).map(Self::new);
            }
            Err(err) => return Err(err.into()),
        };
        file.write_all(&key)?;
        file.sync_all()?;
        Ok(Self::new(key))
    }

    fn new(key: [u8; KEY_LEN]) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new(&Key::from(key)),
        }
    }

    /// Encrypts `plaintext` into the stored text form.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::fill(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: PURPOSE,
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);
        Ok(format!("{VERSION_PREFIX}{}", URL_SAFE_NO_PAD.encode(blob)))
    }

    /// Decrypts a text produced by [`SecretBox::encrypt`].
    pub fn decrypt(&self, stored: &str) -> Result<Redacted<String>, CryptoError> {
        let encoded = stored
            .strip_prefix(VERSION_PREFIX)
            .ok_or(CryptoError::Malformed)?;
        let blob = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CryptoError::Malformed)?;
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::Malformed);
        }
        let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Malformed)?;
        let plaintext = self
            .cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: PURPOSE,
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        String::from_utf8(plaintext)
            .map(Redacted::new)
            .map_err(|_| CryptoError::Decrypt)
    }
}

/// Reads the key file, tightening its permissions when they are looser than `0600`.
fn read_key(path: &Path) -> Result<[u8; KEY_LEN], CryptoError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(CryptoError::MissingKey),
        Err(err) => return Err(err.into()),
    };
    tighten_permissions(&file, path);
    // One byte more than a key, so a file that is too long is noticed.
    let mut bytes = Vec::with_capacity(KEY_LEN + 1);
    file.take(KEY_LEN as u64 + 1).read_to_end(&mut bytes)?;
    bytes.try_into().map_err(|_| CryptoError::BadKey)
}

#[cfg(unix)]
fn tighten_permissions(file: &fs::File, path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = file.metadata() else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return;
    }
    match file.set_permissions(fs::Permissions::from_mode(0o600)) {
        Ok(()) => tracing::warn!(
            path = %path.display(),
            mode = format_args!("{mode:o}"),
            "the Immich key file was readable by others; its mode is now 600"
        ),
        Err(err) => tracing::warn!(
            path = %path.display(),
            mode = format_args!("{mode:o}"),
            error = %err,
            "the Immich key file is readable by others and its mode could not be tightened"
        ),
    }
}

#[cfg(not(unix))]
fn tighten_permissions(_file: &fs::File, _path: &Path) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::HashSet;

    use super::*;
    use crate::immich::test_support::scratch_dir as dir;

    const SECRET: &str = "immich-api-key-0123456789abcdef";

    #[test]
    fn a_secret_round_trips() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        let stored = secrets.encrypt(SECRET).unwrap();
        assert!(stored.starts_with("v1."));
        assert_eq!(secrets.decrypt(&stored).unwrap().expose(), SECRET);

        let reopened = SecretBox::load(dir.path()).unwrap();
        assert_eq!(reopened.decrypt(&stored).unwrap().expose(), SECRET);
    }

    #[test]
    fn the_stored_text_never_contains_the_plaintext() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        let stored = secrets.encrypt(SECRET).unwrap();
        let encoded_plaintext = URL_SAFE_NO_PAD.encode(SECRET);
        assert!(!stored.contains(SECRET));
        assert!(!stored.contains(&encoded_plaintext));
        let key_file = fs::read(key_path(dir.path())).unwrap();
        assert!(!String::from_utf8_lossy(&key_file).contains(SECRET));
    }

    #[test]
    fn every_encryption_uses_a_fresh_nonce() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        let stored: HashSet<String> = (0..200).map(|_| secrets.encrypt(SECRET).unwrap()).collect();
        assert_eq!(stored.len(), 200);
        let nonces: HashSet<Vec<u8>> = stored
            .iter()
            .map(|text| {
                URL_SAFE_NO_PAD
                    .decode(text.strip_prefix(VERSION_PREFIX).unwrap())
                    .unwrap()[..NONCE_LEN]
                    .to_vec()
            })
            .collect();
        assert_eq!(nonces.len(), 200);
    }

    #[test]
    fn tampering_is_detected_in_every_part_of_the_value() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        let stored = secrets.encrypt(SECRET).unwrap();
        let blob = URL_SAFE_NO_PAD
            .decode(stored.strip_prefix(VERSION_PREFIX).unwrap())
            .unwrap();
        // The nonce, the ciphertext and the tag.
        for position in [0, NONCE_LEN, blob.len() - 1] {
            let mut forged = blob.clone();
            forged[position] ^= 0x01;
            let text = format!("{VERSION_PREFIX}{}", URL_SAFE_NO_PAD.encode(forged));
            assert!(
                matches!(secrets.decrypt(&text), Err(CryptoError::Decrypt)),
                "flipping a bit at {position} must be detected"
            );
        }
    }

    #[test]
    fn a_different_key_cannot_decrypt() {
        let (first, second) = (dir(), dir());
        let stored = SecretBox::load_or_create(first.path())
            .unwrap()
            .encrypt(SECRET)
            .unwrap();
        let other = SecretBox::load_or_create(second.path()).unwrap();
        assert!(matches!(other.decrypt(&stored), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn a_value_encrypted_for_another_purpose_is_refused() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        let nonce = [7u8; NONCE_LEN];
        let foreign = secrets
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: SECRET.as_bytes(),
                    aad: b"some other purpose",
                },
            )
            .unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&foreign);
        let text = format!("{VERSION_PREFIX}{}", URL_SAFE_NO_PAD.encode(blob));
        assert!(matches!(secrets.decrypt(&text), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn malformed_values_are_refused() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        for text in ["", "plain", "v2.AAAA", "v1.", "v1.not base64 !!", "v1.AAAA"] {
            assert!(
                matches!(secrets.decrypt(text), Err(CryptoError::Malformed)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_key_file_is_created_with_32_random_bytes_and_mode_600() {
        let dir = dir();
        SecretBox::load_or_create(dir.path()).unwrap();
        let bytes = fs::read(key_path(dir.path())).unwrap();
        assert_eq!(bytes.len(), KEY_LEN);
        assert!(bytes.iter().any(|byte| *byte != 0));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(key_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_existing_key_is_reused_and_keys_differ_between_directories() {
        let (first, second) = (dir(), dir());
        SecretBox::load_or_create(first.path()).unwrap();
        let before = fs::read(key_path(first.path())).unwrap();
        SecretBox::load_or_create(first.path()).unwrap();
        assert_eq!(fs::read(key_path(first.path())).unwrap(), before);

        SecretBox::load_or_create(second.path()).unwrap();
        assert_ne!(fs::read(key_path(second.path())).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_key_with_looser_permissions_is_tightened() {
        use std::os::unix::fs::PermissionsExt;

        type Opener = fn(&Path) -> Result<SecretBox, CryptoError>;
        let openers: [Opener; 2] = [SecretBox::load, SecretBox::load_or_create];
        for open in openers {
            let dir = dir();
            SecretBox::load_or_create(dir.path()).unwrap();
            let path = key_path(dir.path());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            open(dir.path()).unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn load_never_creates_a_key() {
        let dir = dir();
        assert!(matches!(
            SecretBox::load(dir.path()),
            Err(CryptoError::MissingKey)
        ));
        assert!(!key_path(dir.path()).exists());
    }

    #[test]
    fn an_unusable_key_file_is_refused_by_load_and_replaced_by_load_or_create() {
        for content in [&b""[..], &[1u8; 31][..], &[1u8; 33][..]] {
            let dir = dir();
            let path = key_path(dir.path());
            fs::write(&path, content).unwrap();
            assert!(matches!(
                SecretBox::load(dir.path()),
                Err(CryptoError::BadKey)
            ));
            assert_eq!(fs::read(&path).unwrap(), content, "load must not modify it");

            SecretBox::load_or_create(dir.path()).unwrap();
            assert_eq!(fs::read(&path).unwrap().len(), KEY_LEN);
            SecretBox::load(dir.path()).unwrap();
        }
    }

    #[test]
    fn debug_does_not_show_key_material() {
        let dir = dir();
        let secrets = SecretBox::load_or_create(dir.path()).unwrap();
        assert_eq!(format!("{secrets:?}"), "SecretBox { .. }");
    }
}
