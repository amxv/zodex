use std::fmt;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use anyhow::Context as _;
use anyhow::{Result, bail};

const MAX_RUNTIME_KEY_BYTES: usize = 16 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeKey(String);

impl RuntimeKey {
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            bail!("OpenAI tunnel runtime key must not be empty");
        }
        if raw.len() > MAX_RUNTIME_KEY_BYTES {
            bail!("OpenAI tunnel runtime key is unexpectedly large");
        }
        if raw.contains('\0') || raw.contains('\n') || raw.contains('\r') {
            bail!("OpenAI tunnel runtime key must be a single line");
        }
        Ok(Self(raw))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RuntimeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeKey(<redacted>)")
    }
}

pub trait RuntimeKeyStore: Send + Sync {
    fn get(&self) -> Result<Option<RuntimeKey>>;
    fn set(&self, key: &RuntimeKey) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

#[cfg(target_os = "macos")]
pub struct MacKeychainRuntimeKeyStore;

#[cfg(target_os = "windows")]
pub struct WindowsCredentialRuntimeKeyStore;

#[cfg(target_os = "macos")]
impl MacKeychainRuntimeKeyStore {
    const SERVICE: &'static str = "com.amxv.zodex.local";
    const ACCOUNT: &'static str = "openai-tunnel-runtime-key";
    // Security.framework's documented errSecItemNotFound OSStatus.
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;
}

#[cfg(target_os = "macos")]
impl RuntimeKeyStore for MacKeychainRuntimeKeyStore {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        use security_framework::passwords::{PasswordOptions, generic_password};

        match generic_password(PasswordOptions::new_generic_password(
            Self::SERVICE,
            Self::ACCOUNT,
        )) {
            Ok(bytes) => {
                let value = String::from_utf8(bytes)
                    .context("stored OpenAI tunnel runtime key is not valid UTF-8")?;
                Ok(Some(RuntimeKey::new(value)?))
            }
            Err(error) if error.code() == Self::ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(error) => {
                Err(error).context("failed to read OpenAI tunnel runtime key from Keychain")
            }
        }
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        security_framework::passwords::set_generic_password(
            Self::SERVICE,
            Self::ACCOUNT,
            key.expose().as_bytes(),
        )
        .context("failed to store OpenAI tunnel runtime key in Keychain")
    }

    fn delete(&self) -> Result<()> {
        match security_framework::passwords::delete_generic_password(Self::SERVICE, Self::ACCOUNT) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == Self::ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => {
                Err(error).context("failed to remove OpenAI tunnel runtime key from Keychain")
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl WindowsCredentialRuntimeKeyStore {
    const SERVICE: &'static str = "com.amxv.zodex.local";
    const ACCOUNT: &'static str = "openai-tunnel-runtime-key";

    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(Self::SERVICE, Self::ACCOUNT)
            .context("failed to open Windows Credential Manager entry for Zodex Local")
    }
}

#[cfg(target_os = "windows")]
impl RuntimeKeyStore for WindowsCredentialRuntimeKeyStore {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        match Self::entry()?.get_password() {
            Ok(value) => Ok(Some(RuntimeKey::new(value)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context(
                "failed to read OpenAI tunnel runtime key from Windows Credential Manager",
            ),
        }
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        Self::entry()?
            .set_password(key.expose())
            .context("failed to store OpenAI tunnel runtime key in Windows Credential Manager")
    }

    fn delete(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context(
                "failed to remove OpenAI tunnel runtime key from Windows Credential Manager",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeKey;

    #[test]
    fn runtime_key_debug_is_always_redacted() {
        let secret = RuntimeKey::new("sk-secret-fixture").unwrap();
        let debug = format!("{secret:?}");
        assert_eq!(debug, "RuntimeKey(<redacted>)");
        assert!(!debug.contains("sk-secret-fixture"));
    }

    #[test]
    fn runtime_key_rejects_empty_multiline_and_nul_values() {
        for value in ["", "a\nb", "a\rb", "a\0b"] {
            assert!(RuntimeKey::new(value).is_err(), "{value:?} should fail");
        }
    }
}
