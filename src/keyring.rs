// src/keyring.rs
use linux_keyutils::{KeyRing, KeyRingIdentifier, KeyError};
#[allow(unused_imports)]
use linux_raw_sys::errno::{ENOKEY, EKEYREVOKED, EKEYEXPIRED};

/// Linux errno constants from linux-raw-sys for keyring error classification.
/// ENOKEY: Key not available (keyring search found no matching key)
/// EKEYREVOKED: Key has been revoked
/// EKEYEXPIRED: Key has expired
const _ENOKEY: u32 = ENOKEY;
const _EKEYREVOKED: u32 = EKEYREVOKED;
const _EKEYEXPIRED: u32 = EKEYEXPIRED;

/// Attempt to retrieve a secret from the Linux kernel keyring.
/// Searches the session keyring first, then the user keyring.
/// Returns None if the key is not found or the keyring is unavailable.
pub fn try_keyring_secret(key_name: &str) -> Option<String> {
    // Try session keyring first
    if let Some(secret) = search_keyring(KeyRingIdentifier::Session, key_name) {
        return Some(secret);
    }
    // Fall back to user keyring
    if let Some(secret) = search_keyring(KeyRingIdentifier::User, key_name) {
        return Some(secret);
    }
    None
}

/// Search a specific keyring for a key by name and read its payload.
fn search_keyring(keyring_id: KeyRingIdentifier, key_name: &str) -> Option<String> {
    let ring = KeyRing::from_special_id(keyring_id, false).ok()?;
    let key = ring.search(key_name).ok()?;
    let payload = key.read_to_vec().ok()?;
    String::from_utf8(payload).ok()
}

/// Store a secret in the session keyring.
pub fn store_keyring_secret(key_name: &str, secret: &[u8]) -> Result<i32, KeyError> {
    let ring = KeyRing::from_special_id(KeyRingIdentifier::Session, true)?;
    let key = ring.add_key(key_name, secret)?;
    Ok(key.get_id().0)
}

/// Remove a key from the session keyring by name.
pub fn remove_keyring_secret(key_name: &str) -> bool {
    let ring = match KeyRing::from_special_id(KeyRingIdentifier::Session, false) {
        Ok(r) => r,
        Err(_) => return false,
    };
    match ring.search(key_name) {
        Ok(key) => key.invalidate().is_ok(),
        Err(_) => false,
    }
}
