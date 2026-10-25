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

#[cfg(target_os = "macos")]
struct MacKeychainPasswordItem(&'static str);

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
    // The original item was created by ad-hoc-signed binaries on many Macs.
    // Its ACL continues to reference that old code requirement even after
    // upgrades start using a stable local signing identity. A fresh item gets
    // the installed signed operator's requirement at creation time instead.
    const SERVICE: &'static str = "com.amxv.zodex.local.v2";
    const LEGACY_SERVICE: &'static str = "com.amxv.zodex.local";
    const ACCOUNT: &'static str = "openai-tunnel-runtime-key";
    // Security.framework's documented errSecItemNotFound OSStatus.
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;
}

// Keep the legacy item intact until migration succeeds. Do not export the key,
// change the Keychain ACL to allow all applications, or silently swallow a
// failed write and keep re-prompting on every startup.
#[cfg(any(test, target_os = "macos"))]
fn get_or_migrate_runtime_key(
    current: &dyn RuntimeKeyStore,
    legacy: &dyn RuntimeKeyStore,
) -> Result<Option<RuntimeKey>> {
    if let Some(key) = current.get()? {
        return Ok(Some(key));
    }
    let Some(key) = legacy.get()? else {
        return Ok(None);
    };
    current
        .set(&key)
        .context("failed to migrate OpenAI tunnel runtime key into the updated Keychain item")?;
    Ok(Some(key))
}

#[cfg(any(test, target_os = "macos"))]
fn delete_current_and_legacy_runtime_keys(
    current: &dyn RuntimeKeyStore,
    legacy: &dyn RuntimeKeyStore,
) -> Result<()> {
    // Do not stop at the first error; leaving either copy behind after a
    // credential reset would retain a usable tunnel credential.
    let current_result = current.delete();
    let legacy_result = legacy.delete();
    current_result.and(legacy_result)
}

#[cfg(target_os = "macos")]
impl RuntimeKeyStore for MacKeychainRuntimeKeyStore {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        get_or_migrate_runtime_key(
            &MacKeychainPasswordItem(Self::SERVICE),
            &MacKeychainPasswordItem(Self::LEGACY_SERVICE),
        )
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        MacKeychainPasswordItem(Self::SERVICE).set(key)
    }

    fn delete(&self) -> Result<()> {
        delete_current_and_legacy_runtime_keys(
            &MacKeychainPasswordItem(Self::SERVICE),
            &MacKeychainPasswordItem(Self::LEGACY_SERVICE),
        )
    }
}

#[cfg(target_os = "macos")]
impl RuntimeKeyStore for MacKeychainPasswordItem {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        use security_framework::passwords::{PasswordOptions, generic_password};

        match generic_password(PasswordOptions::new_generic_password(
            self.0,
            MacKeychainRuntimeKeyStore::ACCOUNT,
        )) {
            Ok(bytes) => {
                let value = String::from_utf8(bytes)
                    .context("stored OpenAI tunnel runtime key is not valid UTF-8")?;
                Ok(Some(RuntimeKey::new(value)?))
            }
            Err(error) if error.code() == MacKeychainRuntimeKeyStore::ERR_SEC_ITEM_NOT_FOUND => {
                Ok(None)
            }
            Err(error) => {
                Err(error).context("failed to read OpenAI tunnel runtime key from Keychain")
            }
        }
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        security_framework::passwords::set_generic_password(
            self.0,
            MacKeychainRuntimeKeyStore::ACCOUNT,
            key.expose().as_bytes(),
        )
        .context("failed to store OpenAI tunnel runtime key in Keychain")
    }

    fn delete(&self) -> Result<()> {
        match security_framework::passwords::delete_generic_password(
            self.0,
            MacKeychainRuntimeKeyStore::ACCOUNT,
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == MacKeychainRuntimeKeyStore::ERR_SEC_ITEM_NOT_FOUND => {
                Ok(())
            }
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

#[cfg(test)]
mod migration_tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use anyhow::{Result, bail};

    use super::{
        RuntimeKey, RuntimeKeyStore, delete_current_and_legacy_runtime_keys,
        get_or_migrate_runtime_key,
    };

    #[derive(Default)]
    struct FakeStore {
        value: Mutex<Option<RuntimeKey>>,
        reads: AtomicUsize,
        writes: AtomicUsize,
        reject_read: bool,
        reject_write: bool,
        reject_delete: bool,
    }

    impl FakeStore {
        fn with_key(raw: &str) -> Self {
            Self {
                value: Mutex::new(Some(RuntimeKey::new(raw).unwrap())),
                ..Self::default()
            }
        }
    }

    impl RuntimeKeyStore for FakeStore {
        fn get(&self) -> Result<Option<RuntimeKey>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.reject_read {
                bail!("read denied");
            }
            Ok(self.value.lock().unwrap().clone())
        }

        fn set(&self, key: &RuntimeKey) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.reject_write {
                bail!("write denied");
            }
            *self.value.lock().unwrap() = Some(key.clone());
            Ok(())
        }

        fn delete(&self) -> Result<()> {
            if self.reject_delete {
                bail!("delete denied");
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn existing_signed_item_never_reads_or_overwrites_legacy_item() {
        let signed = FakeStore::with_key("current");
        let legacy = FakeStore::with_key("old");
        assert_eq!(
            get_or_migrate_runtime_key(&signed, &legacy).unwrap(),
            Some(RuntimeKey::new("current").unwrap())
        );
        assert_eq!(signed.writes.load(Ordering::SeqCst), 0);
        assert_eq!(legacy.reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn legacy_item_is_copied_once_without_deleting_the_rollback_copy() {
        let signed = FakeStore::default();
        let legacy = FakeStore::with_key("old");
        let expected = Some(RuntimeKey::new("old").unwrap());

        assert_eq!(
            get_or_migrate_runtime_key(&signed, &legacy).unwrap(),
            expected
        );
        assert_eq!(signed.writes.load(Ordering::SeqCst), 1);
        assert_eq!(legacy.get().unwrap(), expected);

        assert_eq!(
            get_or_migrate_runtime_key(&signed, &legacy).unwrap(),
            expected
        );
        assert_eq!(signed.writes.load(Ordering::SeqCst), 1);
        assert_eq!(legacy.reads.load(Ordering::SeqCst), 2); // explicit check only
    }

    #[test]
    fn missing_both_items_does_not_create_a_credential() {
        let signed = FakeStore::default();
        let legacy = FakeStore::default();
        assert_eq!(get_or_migrate_runtime_key(&signed, &legacy).unwrap(), None);
        assert_eq!(signed.writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn migration_failure_preserves_the_legacy_key_and_surfaces_an_error() {
        let signed = FakeStore {
            reject_write: true,
            ..FakeStore::default()
        };
        let legacy = FakeStore::with_key("old");
        let error = get_or_migrate_runtime_key(&signed, &legacy).unwrap_err();
        assert!(error.to_string().contains("failed to migrate"));
        assert!(!error.to_string().contains("old"));
        assert_eq!(legacy.get().unwrap(), Some(RuntimeKey::new("old").unwrap()));
    }

    #[test]
    fn signed_item_read_errors_do_not_fall_back_to_stale_legacy_key() {
        let signed = FakeStore {
            reject_read: true,
            ..FakeStore::default()
        };
        let legacy = FakeStore::with_key("stale");
        assert!(get_or_migrate_runtime_key(&signed, &legacy).is_err());
        assert_eq!(legacy.reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reset_removes_both_items() {
        let signed = FakeStore::with_key("new");
        let legacy = FakeStore::with_key("old");
        delete_current_and_legacy_runtime_keys(&signed, &legacy).unwrap();
        assert_eq!(signed.get().unwrap(), None);
        assert_eq!(legacy.get().unwrap(), None);
    }

    #[test]
    fn reset_attempts_legacy_deletion_even_if_primary_delete_fails() {
        let signed = FakeStore {
            reject_delete: true,
            ..FakeStore::with_key("new")
        };
        let legacy = FakeStore::with_key("old");
        assert!(delete_current_and_legacy_runtime_keys(&signed, &legacy).is_err());
        assert_eq!(signed.get().unwrap(), Some(RuntimeKey::new("new").unwrap()));
        assert_eq!(legacy.get().unwrap(), None);
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
