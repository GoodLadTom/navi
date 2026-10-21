//! API keys and tokens, kept in the Windows Credential Manager or, on a Mac, the login Keychain
//! (never in settings.json or logs).
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! Contract: service "Navi", user = the secret name ("anthropic", "openai", "custom", "clickup",
//! "notion"). `get` returns None when missing. `set` with an empty value deletes.
//!
//! Notes
//! * Values are trimmed before saving: a key pasted with a trailing newline or space would otherwise
//!   be rejected by the provider for no visible reason. A value that is only whitespace deletes.
//! * Windows keeps a credential's secret in 2,560 bytes, so a value longer than about 1,280
//!   characters is refused with an error rather than saved cut short.
//! * Nothing here ever logs or returns a secret except `get`, and error messages name the secret
//!   ("anthropic"), never its value.

use anyhow::{anyhow, bail, Result};
use keyring::Entry;

/// Where saved keys live, as the user knows it.
#[cfg(target_os = "macos")]
const STORE: &str = "Keychain";
#[cfg(not(target_os = "macos"))]
const STORE: &str = "Windows Credential Manager";

/// The service name every Navi credential is filed under.
const SERVICE: &str = "Navi";

fn entry(name: &str) -> Result<Entry> {
    let name = name.trim();
    if name.is_empty() {
        bail!("A saved key needs a name.");
    }
    Entry::new(SERVICE, name).map_err(|e| anyhow!("Couldn't reach the {STORE} for \"{name}\": {e}"))
}

/// Saves `value` under `name`, replacing any earlier value. An empty value deletes the entry.
pub fn set(name: &str, value: &str) -> Result<()> {
    let entry = entry(name)?;
    let name = name.trim();
    let value = value.trim();
    if value.is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!("Couldn't remove the saved key \"{name}\": {e}")),
        };
    }
    entry.set_password(value).map_err(|e| anyhow!("Couldn't save the key \"{name}\" in the {STORE}: {e}"))
}

/// The saved value, or None if there is none (or the Credential Manager cannot be read).
pub fn get(name: &str) -> Option<String> {
    let entry = entry(name).ok()?;
    match entry.get_password() {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            log::warn!("could not read the saved key \"{}\": {e}", name.trim());
            None
        }
    }
}

pub fn has(name: &str) -> bool {
    get(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one credential the tests may touch. It is removed when the test ends, even on failure.
    const TEST_NAME: &str = "navi-test-secret";

    struct RemoveTestCredential;

    impl Drop for RemoveTestCredential {
        fn drop(&mut self) {
            if let Ok(entry) = Entry::new(SERVICE, TEST_NAME) {
                let _ = entry.delete_credential();
            }
        }
    }

    #[test]
    fn a_secret_round_trips_through_the_windows_credential_manager() {
        let _cleanup = RemoveTestCredential;
        // Start clean in case an earlier run was interrupted.
        set(TEST_NAME, "").unwrap();
        assert_eq!(get(TEST_NAME), None);
        assert!(!has(TEST_NAME));

        set(TEST_NAME, "sk-test-first").unwrap();
        assert_eq!(get(TEST_NAME).as_deref(), Some("sk-test-first"));
        assert!(has(TEST_NAME));

        set(TEST_NAME, "sk-test-second").unwrap();
        assert_eq!(get(TEST_NAME).as_deref(), Some("sk-test-second"), "saving again replaces the value");

        set(TEST_NAME, "  padded-key \r\n").unwrap();
        assert_eq!(get(TEST_NAME).as_deref(), Some("padded-key"), "pasted whitespace is trimmed");

        set(TEST_NAME, "kéy-ñ-✓-日本").unwrap();
        assert_eq!(get(TEST_NAME).as_deref(), Some("kéy-ñ-✓-日本"), "non-ASCII survives");

        // Only Windows caps a credential's size; the Keychain takes it.
        #[cfg(windows)]
        {
            let too_long = "x".repeat(2_000);
            let err = set(TEST_NAME, &too_long).unwrap_err();
            let message = format!("{err:#}");
            assert!(message.contains(TEST_NAME), "{message}");
            assert!(!message.contains("xxxxxxxx"), "an error must never contain the secret");
            assert_eq!(get(TEST_NAME).as_deref(), Some("kéy-ñ-✓-日本"), "a refused save leaves the old value alone");
        }

        set(TEST_NAME, "   ").unwrap();
        assert_eq!(get(TEST_NAME), None, "whitespace only deletes");
        assert!(!has(TEST_NAME));

        set(TEST_NAME, "sk-test-third").unwrap();
        set(TEST_NAME, "").unwrap();
        assert_eq!(get(TEST_NAME), None, "an empty value deletes");
        set(TEST_NAME, "").unwrap();
    }

    #[test]
    fn a_secret_needs_a_name() {
        assert!(set("", "value").is_err());
        assert!(set("   ", "").is_err());
        assert_eq!(get(""), None);
        assert!(!has("  "));
    }
}
