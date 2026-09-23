use std::fmt;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

#[cfg(target_os = "linux")]
pub struct LinuxFileRuntimeKeyStore {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl LinuxFileRuntimeKeyStore {
    pub fn new(paths: &super::LocalPaths) -> Self {
        Self {
            path: paths.runtime_key_file(),
        }
    }
}

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

#[cfg(target_os = "linux")]
impl RuntimeKeyStore for LinuxFileRuntimeKeyStore {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        if !self.path.exists() {
            return Ok(None);
        }
        super::private_fs::verify_user_only_file(&self.path)?;
        let value = fs::read_to_string(&self.path).with_context(|| {
            format!(
                "failed to read OpenAI tunnel runtime key from {}",
                self.path.display()
            )
        })?;
        Ok(Some(RuntimeKey::new(value)?))
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("Linux runtime-key path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Linux credential directory {}",
                parent.display()
            )
        })?;
        super::private_fs::set_user_only_directory(parent)?;

        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .context("failed to create temporary Linux runtime-key file")?;
        super::private_fs::set_user_only_file(temp.path())?;
        temp.write_all(key.expose().as_bytes())
            .context("failed to write Linux runtime-key file")?;
        temp.as_file()
            .sync_all()
            .context("failed to sync Linux runtime-key file")?;
        temp.persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "failed to persist OpenAI tunnel runtime key at {}",
                    self.path.display()
                )
            })?;
        super::private_fs::set_user_only_file(&self.path)?;
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to remove OpenAI tunnel runtime key from {}",
                    self.path.display()
                )
            }),
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

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::{LinuxFileRuntimeKeyStore, RuntimeKey, RuntimeKeyStore};
    use crate::local::LocalPaths;

    #[test]
    fn linux_runtime_key_store_is_user_only_and_round_trips() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        let store = LinuxFileRuntimeKeyStore::new(&paths);
        let key = RuntimeKey::new("linux-runtime-secret").unwrap();

        assert!(store.get().unwrap().is_none());
        store.set(&key).unwrap();
        assert_eq!(store.get().unwrap(), Some(key));
        assert_eq!(
            std::fs::metadata(paths.runtime_key_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        store.delete().unwrap();
        assert!(store.get().unwrap().is_none());
    }
}
