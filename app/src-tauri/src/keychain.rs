//! macOS keychain access via the `keyring` crate. Service "dev.vigie.app",
//! one item per account (`account <login>`) plus the legacy pre-v5 item
//! (`account github-token`) — see docs/CONTRACT.md "Accounts".
//!
//! ## Mocking
//!
//! `keyring`'s `Entry` has no portable "list every item for this service"
//! call, so [`super::install_account_tokens`] gets its list of logins from
//! the engine (`list_accounts`) rather than from the keychain itself — the
//! packet's fallback for a crate that can't enumerate.
//!
//! Under `--features mock`, every function below is backed by an in-memory
//! map instead of the real keychain: the packet forbids exercising the real
//! keychain outside `cargo test --features mock`, and this is what makes
//! that true rather than aspirational — a mock-feature test can call
//! `store_token_for`/`load_token_for`/`delete_token_for` (or drive
//! `lib.rs`'s startup helpers, which call the same functions) without ever
//! touching `Entry` or the FFI below. The real backend's ACL/FFI handling
//! (next section) is therefore compiled out entirely under `mock` — there
//! is nothing there to fall back to.
//!
//! ## Why there is FFI in the real backend
//!
//! A macOS generic-password item carries an ACL naming the binaries allowed
//! to decrypt it, matched by code signature. Re-signing Vigie (ad-hoc dev
//! build → Developer ID) makes it a *different* binary to the keychain, so
//! an item an earlier build wrote is no longer readable or writable by the
//! new one, and `keyring` cannot get past that on its own:
//!
//!   * `Entry::set_password` is `find_generic_password` (which decrypts) and,
//!     only if that fails, `SecKeychainAddGenericPassword`. The find is
//!     refused with `errSecAuthFailed` (-25293) and the add then hits the
//!     stale item as `errSecDuplicateItem` (-25299).
//!   * `Entry::delete_credential` *also* goes through `find_generic_password`,
//!     so it fails with the same -25293 — keyring cannot delete an item it
//!     is not allowed to read.
//!
//! `SecKeychainFindGenericPassword` with null password out-parameters looks
//! the item up *without* decrypting it, which no ACL guards, and
//! `SecKeychainItemDelete` on that reference succeeds. So [`purge_item`]
//! clears the foreign item and the ordinary `set_password` that follows
//! creates a fresh one owned by the running build. All three statuses above
//! were reproduced against an item whose ACL trusted a different binary;
//! see the packet report.

// Only the real (non-mock) backend below reads this.
#[allow(dead_code)]
const SERVICE: &str = "dev.vigie.app";

/// The pre-v5 single-account keychain item's account name. Migrated to a
/// proper account at startup and never written to again — see
/// [`load_token`]/[`clear_token`], kept for exactly that legacy item.
const LEGACY_ACCOUNT: &str = "github-token";

/// Every account gets its own keychain item, looked up by login.
pub fn load_token_for(login: &str) -> Result<Option<String>, String> {
    backend::get(login)
}

/// Stores (replacing) the token for one account.
pub fn store_token_for(login: &str, token: &str) -> Result<(), String> {
    backend::set(login, token)
}

/// Removes one account's item. Deleting one that doesn't exist is success —
/// the end state (no stored token for that login) is what the caller wants
/// either way.
pub fn delete_token_for(login: &str) -> Result<(), String> {
    backend::delete(login)
}

/// Returns `Ok(None)` when the legacy item was never there (or has already
/// been migrated), rather than surfacing that as a failure.
pub fn load_token() -> Result<Option<String>, String> {
    load_token_for(LEGACY_ACCOUNT)
}

/// Deleting a credential that doesn't exist is treated as success — see
/// [`delete_token_for`].
pub fn clear_token() -> Result<(), String> {
    delete_token_for(LEGACY_ACCOUNT)
}

#[cfg(not(feature = "mock"))]
mod backend {
    use std::error::Error as _;
    use std::ffi::c_void;

    use keyring::Entry;

    use super::SERVICE;

    /// `errSecItemNotFound` — nothing to delete, which [`purge_item`] treats
    /// as success.
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainFindGenericPassword(
            keychain_or_array: *const c_void,
            service_name_len: u32,
            service_name: *const u8,
            account_name_len: u32,
            account_name: *const u8,
            password_len: *mut u32,
            password_data: *mut *mut c_void,
            item_ref: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainItemDelete(item_ref: *mut c_void) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    fn entry(account: &str) -> Result<Entry, String> {
        Entry::new(SERVICE, account).map_err(|e| format!("keychain unavailable: {e}"))
    }

    /// A keyring error with the underlying OSStatus attached.
    ///
    /// `keyring::Error`'s own `Display` is only the Security framework's
    /// human-readable message ("The specified item already exists in the
    /// keychain."), which is not enough to tell -25299 from -25293 in a bug
    /// report. The source error's `Debug` carries the numeric code, so both go
    /// into the string the UI shows.
    fn describe(e: &keyring::Error) -> String {
        match e.source() {
            Some(source) => format!("{e} [{source:?}]"),
            None => e.to_string(),
        }
    }

    /// Deletes the `SERVICE`/`account` item without reading it.
    ///
    /// Passing null for the password out-parameters is what makes this work on
    /// an item this build is not in the ACL of: the lookup never decrypts, so
    /// macOS neither prompts nor refuses. `Ok(())` when the item is gone —
    /// including when there was none to begin with.
    fn purge_item(account: &str) -> Result<(), String> {
        unsafe {
            let mut item: *mut c_void = std::ptr::null_mut();
            let found = SecKeychainFindGenericPassword(
                std::ptr::null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr(),
                account.len() as u32,
                account.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut item,
            );
            if found == ERR_SEC_ITEM_NOT_FOUND {
                return Ok(());
            }
            if found != 0 {
                return Err(format!("couldn't find the existing keychain item (OSStatus {found})"));
            }
            let deleted = SecKeychainItemDelete(item);
            CFRelease(item);
            if deleted != 0 {
                return Err(format!("couldn't delete the existing keychain item (OSStatus {deleted})"));
            }
            Ok(())
        }
    }

    /// Stores the token, replacing whatever is there.
    ///
    /// The straightforward write is tried first; only if it fails does this fall
    /// back to deleting the existing item and adding a fresh one, because that
    /// path throws away a credential and should never run speculatively. Both
    /// failures are reported together when the fallback does not rescue it — the
    /// first error says what the keychain objected to, the second says what
    /// happened when we tried to start over.
    pub fn set(account: &str, token: &str) -> Result<(), String> {
        let first = match entry(account)?.set_password(token) {
            Ok(()) => return Ok(()),
            Err(e) => describe(&e),
        };
        if let Err(purge) = purge_item(account) {
            return Err(format!("couldn't save token: {first}; {purge}"));
        }
        entry(account)?.set_password(token).map_err(|e| {
            format!(
                "couldn't save token even after clearing the old keychain item: {} (first attempt: {first})",
                describe(&e)
            )
        })
    }

    /// Returns `Ok(None)` when no token has ever been stored (keyring's
    /// `NoEntry` error), rather than surfacing that as a failure.
    pub fn get(account: &str) -> Result<Option<String>, String> {
        match entry(account)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("couldn't read token: {}", describe(&e))),
        }
    }

    /// Falls back to [`purge_item`] for the same reason [`set`] does:
    /// keyring's delete decrypts the item first, so it cannot remove one written
    /// by a differently-signed build, and leaving the credential behind is
    /// worse than skipping the read.
    pub fn delete(account: &str) -> Result<(), String> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => purge_item(account).map_err(|purge| {
                format!("couldn't clear token: {}; {purge}", describe(&e))
            }),
        }
    }
}

/// In-memory stand-in for the real keychain, keyed by account name exactly
/// like the real one (`SERVICE` is implicit — the mock only ever runs as
/// one process's worth of one app, so there is nothing else it could
/// collide with). A `static` rather than engine-owned state because the
/// keychain itself is process-global on macOS too: nothing in this module
/// is threaded through from the caller.
#[cfg(feature = "mock")]
mod backend {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Mutex, OnceLock};

    fn store() -> &'static Mutex<HashMap<String, String>> {
        static STORE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        STORE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Accounts whose next `set` call should fail instead of writing — test
    /// fault injection only, see [`super::fail_next_write_for`]. Consumed on
    /// use, so a poisoned write fails exactly once.
    fn poisoned() -> &'static Mutex<HashSet<String>> {
        static POISONED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        POISONED.get_or_init(|| Mutex::new(HashSet::new()))
    }

    pub fn get(account: &str) -> Result<Option<String>, String> {
        Ok(store().lock().unwrap().get(account).cloned())
    }

    pub fn set(account: &str, token: &str) -> Result<(), String> {
        if poisoned().lock().unwrap().remove(account) {
            return Err(format!("mock keychain write failure injected for {account}"));
        }
        store().lock().unwrap().insert(account.to_string(), token.to_string());
        Ok(())
    }

    pub fn delete(account: &str) -> Result<(), String> {
        store().lock().unwrap().remove(account);
        Ok(())
    }

    #[cfg(test)]
    pub fn fail_next_write_for(account: &str) {
        poisoned().lock().unwrap().insert(account.to_string());
    }
}

/// Test-only fault injection: makes the next `store_token_for(account, _)`
/// call fail once, so callers that must survive a transient keychain write
/// failure (`lib.rs`'s `migrate_legacy_token`, chiefly) have something to
/// exercise under `--features mock`.
#[cfg(all(test, feature = "mock"))]
pub(crate) fn fail_next_write_for(account: &str) {
    backend::fail_next_write_for(account);
}

/// The mock backend is one process-wide map (macOS's real keychain is
/// process-global too, which is what it stands in for), so any test outside
/// this file that also drives it — `lib.rs`'s startup tests, chiefly —
/// takes this lock first. `cargo test` runs tests in parallel by default;
/// without this, two tests touching the same login (`LEGACY_ACCOUNT` most
/// of all, since every migration test needs it) could interleave.
#[cfg(all(test, feature = "mock"))]
pub(crate) fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(all(test, feature = "mock"))]
mod tests {
    use super::*;

    #[test]
    fn per_account_items_round_trip_and_stay_isolated() {
        let _guard = lock_for_test();
        delete_token_for("kc-test-alice").unwrap();
        delete_token_for("kc-test-bob").unwrap();

        assert_eq!(load_token_for("kc-test-alice").unwrap(), None);
        store_token_for("kc-test-alice", "tok-alice").unwrap();
        store_token_for("kc-test-bob", "tok-bob").unwrap();
        assert_eq!(load_token_for("kc-test-alice").unwrap(), Some("tok-alice".to_string()));
        assert_eq!(load_token_for("kc-test-bob").unwrap(), Some("tok-bob".to_string()));

        delete_token_for("kc-test-alice").unwrap();
        assert_eq!(load_token_for("kc-test-alice").unwrap(), None);
        // Deleting an account that was never there is success, not an error.
        delete_token_for("kc-test-alice").unwrap();
        // Deleting one account's item never touches another's.
        assert_eq!(load_token_for("kc-test-bob").unwrap(), Some("tok-bob".to_string()));

        delete_token_for("kc-test-bob").unwrap();
    }

    #[test]
    fn the_legacy_item_is_independent_of_any_account_item() {
        let _guard = lock_for_test();
        store_token_for(LEGACY_ACCOUNT, "legacy-tok").unwrap();
        assert_eq!(load_token().unwrap(), Some("legacy-tok".to_string()));
        assert_eq!(load_token_for("kc-test-someone"), Ok(None));

        clear_token().unwrap();
        assert_eq!(load_token().unwrap(), None);
    }
}
