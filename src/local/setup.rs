use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tempfile::{NamedTempFile, TempDir};

use super::{
    ArchiveExtractor, LocalConfig, LocalPaths, ManagedTunnelClientRelease,
    OfficialTunnelReleaseClient, ResolvedTunnelRelease, RuntimeKey, RuntimeKeyStore,
    TunnelArchitecture, TunnelMetadataValidator, ensure_offline_mutation, sha256_hex,
    validate_tunnel_id,
};

#[derive(Debug, Clone)]
pub struct LocalSetupRequest {
    pub tunnel_id: String,
    pub runtime_key: RuntimeKey,
    pub architecture: TunnelArchitecture,
    pub rotate_observability_bearer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSetupResult {
    pub tunnel_id: String,
    pub release_version: String,
    pub managed_binary: PathBuf,
    pub binary_updated: bool,
    pub observability_bearer_rotated: bool,
}

pub struct LocalSetupService<'a> {
    paths: &'a LocalPaths,
    releases: &'a OfficialTunnelReleaseClient,
    extractor: &'a dyn ArchiveExtractor,
    validator: &'a dyn TunnelMetadataValidator,
    secrets: &'a dyn RuntimeKeyStore,
}

impl<'a> LocalSetupService<'a> {
    pub fn new(
        paths: &'a LocalPaths,
        releases: &'a OfficialTunnelReleaseClient,
        extractor: &'a dyn ArchiveExtractor,
        validator: &'a dyn TunnelMetadataValidator,
        secrets: &'a dyn RuntimeKeyStore,
    ) -> Self {
        Self {
            paths,
            releases,
            extractor,
            validator,
            secrets,
        }
    }

    pub async fn run(&self, request: LocalSetupRequest) -> Result<LocalSetupResult> {
        ensure_offline_mutation(self.paths, "run Local setup")?;
        validate_tunnel_id(&request.tunnel_id)?;
        self.paths.ensure_persistent_dirs()?;

        let old_config = LocalConfig::load(&self.paths.config_file())?;
        let release = self.releases.resolve_latest(request.architecture).await?;
        let prepared = self.prepare_tunnel_client(&old_config, &release).await?;

        self.validator
            .validate(
                prepared.validation_path(),
                &request.tunnel_id,
                &request.runtime_key,
            )
            .context(
                "read-only tunnel metadata validation failed; no Local setup state was changed",
            )?;

        let old_secret = self.secrets.get().context(
            "failed to snapshot the current secure runtime key before Local setup mutation",
        )?;
        let snapshots = SetupSnapshots::capture(self.paths)?;
        let binary_updated = prepared.is_staged();

        let mut new_config = old_config;
        new_config.tunnel.id = Some(request.tunnel_id.clone());
        new_config.tunnel.client_path = Some(self.paths.managed_tunnel_client());
        new_config.tunnel.release = Some(prepared.metadata().clone());
        let rendered_config = toml::to_string_pretty(&new_config)
            .context("failed to serialize Local setup config")?;
        if rendered_config.contains(request.runtime_key.expose()) {
            bail!("internal safety check refused to serialize the tunnel runtime key");
        }

        let commit = self.commit(
            &request,
            &new_config,
            prepared,
            &snapshots,
            old_secret.as_ref(),
        );
        let observability_bearer_rotated = match commit {
            Ok(rotated) => rotated,
            Err(error) => return Err(error),
        };

        Ok(LocalSetupResult {
            tunnel_id: request.tunnel_id,
            release_version: release.version,
            managed_binary: self.paths.managed_tunnel_client(),
            binary_updated,
            observability_bearer_rotated,
        })
    }

    async fn prepare_tunnel_client(
        &self,
        config: &LocalConfig,
        release: &ResolvedTunnelRelease,
    ) -> Result<PreparedTunnelClient> {
        let destination = self.paths.managed_tunnel_client();
        if let Some(metadata) = reusable_managed_binary(config, &destination, release)? {
            return Ok(PreparedTunnelClient::Existing {
                path: destination,
                metadata,
            });
        }

        let archive = self.releases.download_verified_archive(release).await?;
        let parent = destination
            .parent()
            .context("managed tunnel-client path must have a parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create managed tunnel-client directory {}",
                parent.display()
            )
        })?;
        let stage = tempfile::Builder::new()
            .prefix("setup-")
            .tempdir_in(parent)
            .context("failed to create managed tunnel-client staging directory")?;
        let archive_path = stage.path().join("archive.zip");
        fs::write(&archive_path, &archive)
            .context("failed to stage verified tunnel-client archive")?;
        let bundle_dir = stage.path().join("bundle");
        self.extractor
            .extract_tunnel_bundle(&archive_path, &bundle_dir)
            .context("failed to extract verified tunnel-client archive")?;
        let binary_path = bundle_dir.join(format!("tunnel-client{}", std::env::consts::EXE_SUFFIX));
        let cloudflared_path =
            bundle_dir.join(format!("cloudflared{}", std::env::consts::EXE_SUFFIX));
        let cloudflared_manifest_path = bundle_dir.join("cloudflared-manifest.json");
        if !binary_path.is_file() {
            bail!("tunnel-client extractor did not produce a regular binary file");
        }
        if !cloudflared_path.is_file() {
            bail!("tunnel-client extractor did not produce the bundled cloudflared companion");
        }
        if !cloudflared_manifest_path.is_file() {
            bail!("tunnel-client extractor did not produce the bundled cloudflared manifest");
        }
        let binary_sha256 = sha256_file(&binary_path)?;
        let cloudflared_sha256 = sha256_file(&cloudflared_path)?;
        let cloudflared_manifest_sha256 = sha256_file(&cloudflared_manifest_path)?;
        let metadata = release.managed_metadata(
            binary_sha256,
            cloudflared_sha256,
            cloudflared_manifest_sha256,
        );
        Ok(PreparedTunnelClient::Staged {
            _stage: stage,
            path: binary_path,
            cloudflared_path,
            cloudflared_manifest_path,
            metadata,
        })
    }

    fn commit(
        &self,
        request: &LocalSetupRequest,
        config: &LocalConfig,
        prepared: PreparedTunnelClient,
        snapshots: &SetupSnapshots,
        old_secret: Option<&RuntimeKey>,
    ) -> Result<bool> {
        if let Err(error) = self.secrets.set(&request.runtime_key) {
            let restore = match old_secret {
                Some(key) => self.secrets.set(key),
                None => self.secrets.delete(),
            };
            return match restore {
                Ok(()) => Err(error.context(
                    "failed to store OpenAI tunnel runtime key; prior secure credential state was restored",
                )),
                Err(restore_error) => Err(anyhow!(
                    "failed to store OpenAI tunnel runtime key and failed to restore the prior secure credential state: {error:#}; restore: {restore_error:#}"
                )),
            };
        }

        let operation = (|| {
            if let PreparedTunnelClient::Staged {
                path,
                cloudflared_path,
                cloudflared_manifest_path,
                ..
            } = &prepared
            {
                // Keep the executable named in config as the last bundle member replaced.
                // If setup is interrupted, the next run's hash checks reject the mixed bundle.
                replace_staged_file(
                    cloudflared_manifest_path,
                    &self.paths.managed_cloudflared_manifest(),
                    "cloudflared manifest",
                )?;
                replace_staged_file(
                    cloudflared_path,
                    &self.paths.managed_cloudflared(),
                    "cloudflared",
                )?;
                replace_staged_file(path, &self.paths.managed_tunnel_client(), "tunnel-client")?;
            }
            config.save(&self.paths.config_file())?;
            ensure_observability_bearer(self.paths, request.rotate_observability_bearer)
        })();

        match operation {
            Ok(rotated) => Ok(rotated),
            Err(error) => {
                let rollback = self.rollback(snapshots, old_secret);
                match rollback {
                    Ok(()) => {
                        Err(error
                            .context("Local setup commit failed; prior setup state was restored"))
                    }
                    Err(rollback_error) => Err(anyhow!(
                        "Local setup commit failed and rollback also failed; setup may require repair: {error:#}; rollback: {rollback_error:#}"
                    )),
                }
            }
        }
    }

    fn rollback(&self, snapshots: &SetupSnapshots, old_secret: Option<&RuntimeKey>) -> Result<()> {
        let mut failures = Vec::new();
        for (path, snapshot) in [
            (&self.paths.config_file(), &snapshots.config),
            (&self.paths.managed_tunnel_client(), &snapshots.binary),
            (&self.paths.managed_cloudflared(), &snapshots.cloudflared),
            (
                &self.paths.managed_cloudflared_manifest(),
                &snapshots.cloudflared_manifest,
            ),
            (&self.paths.observability_bearer_file(), &snapshots.bearer),
        ] {
            if let Err(error) = restore_file(path, snapshot.as_ref()) {
                failures.push(format!("{}: {error:#}", path.display()));
            }
        }

        let secret_restore = match old_secret {
            Some(key) => self.secrets.set(key),
            None => self.secrets.delete(),
        };
        if let Err(error) = secret_restore {
            failures.push(format!("secure runtime key: {error:#}"));
        }

        if failures.is_empty() {
            Ok(())
        } else {
            bail!("{}", failures.join("; "))
        }
    }
}

enum PreparedTunnelClient {
    Existing {
        path: PathBuf,
        metadata: ManagedTunnelClientRelease,
    },
    Staged {
        _stage: TempDir,
        path: PathBuf,
        cloudflared_path: PathBuf,
        cloudflared_manifest_path: PathBuf,
        metadata: ManagedTunnelClientRelease,
    },
}

impl PreparedTunnelClient {
    fn validation_path(&self) -> &Path {
        match self {
            Self::Existing { path, .. } | Self::Staged { path, .. } => path,
        }
    }

    fn metadata(&self) -> &ManagedTunnelClientRelease {
        match self {
            Self::Existing { metadata, .. } | Self::Staged { metadata, .. } => metadata,
        }
    }

    fn is_staged(&self) -> bool {
        matches!(self, Self::Staged { .. })
    }
}

struct SetupSnapshots {
    config: Option<FileSnapshot>,
    binary: Option<FileSnapshot>,
    cloudflared: Option<FileSnapshot>,
    cloudflared_manifest: Option<FileSnapshot>,
    bearer: Option<FileSnapshot>,
}

impl SetupSnapshots {
    fn capture(paths: &LocalPaths) -> Result<Self> {
        Ok(Self {
            config: capture_file(&paths.config_file())?,
            binary: capture_file(&paths.managed_tunnel_client())?,
            cloudflared: capture_file(&paths.managed_cloudflared())?,
            cloudflared_manifest: capture_file(&paths.managed_cloudflared_manifest())?,
            bearer: capture_file(&paths.observability_bearer_file())?,
        })
    }
}

#[derive(Clone)]
struct FileSnapshot {
    bytes: Vec<u8>,
    mode: u32,
}

pub fn ensure_observability_bearer(paths: &LocalPaths, rotate: bool) -> Result<bool> {
    let path = paths.observability_bearer_file();
    if path.exists() && !rotate {
        let existing = fs::read_to_string(&path).with_context(|| {
            format!("failed to read observability bearer at {}", path.display())
        })?;
        if existing.trim().len() < 32 {
            bail!(
                "existing observability bearer at {} is invalid; rerun setup with --rotate-observability-bearer",
                path.display()
            );
        }
        set_user_only_permissions(&path)?;
        return Ok(false);
    }

    let random = rand::random::<[u8; 32]>();
    let bearer = URL_SAFE_NO_PAD.encode(random);
    atomic_write(&path, bearer.as_bytes(), 0o600)?;
    Ok(true)
}

fn reusable_managed_binary(
    config: &LocalConfig,
    destination: &Path,
    release: &ResolvedTunnelRelease,
) -> Result<Option<ManagedTunnelClientRelease>> {
    let Some(metadata) = config.tunnel.release.as_ref() else {
        return Ok(None);
    };
    if config.tunnel.client_path.as_deref() != Some(destination)
        || metadata.version != release.version
        || metadata.asset_name != release.asset_name
        || metadata.archive_sha256 != release.archive_sha256
        || !destination.is_file()
        || !destination
            .with_file_name(format!("cloudflared{}", std::env::consts::EXE_SUFFIX))
            .is_file()
        || !destination
            .with_file_name("cloudflared-manifest.json")
            .is_file()
    {
        return Ok(None);
    }
    if sha256_file(destination)? != metadata.binary_sha256 {
        return Ok(None);
    }
    if sha256_file(
        &destination.with_file_name(format!("cloudflared{}", std::env::consts::EXE_SUFFIX)),
    )? != metadata.cloudflared_sha256
    {
        return Ok(None);
    }
    if sha256_file(&destination.with_file_name("cloudflared-manifest.json"))?
        != metadata.cloudflared_manifest_sha256
    {
        return Ok(None);
    }
    let mut current = metadata.clone();
    current.source_url = release.archive_url.clone();
    Ok(Some(current))
}

fn replace_staged_file(staged: &Path, destination: &Path, label: &str) -> Result<()> {
    let parent = destination
        .parent()
        .with_context(|| format!("managed {label} destination must have a parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::rename(staged, destination).with_context(|| {
        format!(
            "failed to atomically replace managed {label} {}",
            destination.display()
        )
    })?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read {} for SHA-256", path.display()))?;
    if bytes.is_empty() {
        bail!("managed tunnel-client binary is empty: {}", path.display());
    }
    Ok(sha256_hex(&bytes))
}

fn capture_file(path: &Path) -> Result<Option<FileSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to snapshot {}", path.display()))?;
    let mode = file_mode(path)?;
    Ok(Some(FileSnapshot { bytes, mode }))
}

fn restore_file(path: &Path, snapshot: Option<&FileSnapshot>) -> Result<()> {
    match snapshot {
        Some(snapshot) => atomic_write(path, &snapshot.bytes, snapshot.mode),
        None => {
            if path.exists() {
                fs::remove_file(path).with_context(|| {
                    format!("failed to remove new setup file {}", path.display())
                })?;
            }
            Ok(())
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .context("Local setup file path must have a parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local setup directory {}",
            parent.display()
        )
    })?;
    let mut temp = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary setup file in {}",
            parent.display()
        )
    })?;
    set_mode(temp.path(), mode)?;
    temp.write_all(bytes)
        .context("failed to write temporary Local setup file")?;
    temp.as_file()
        .sync_all()
        .context("failed to sync temporary Local setup file")?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to persist Local setup file at {}", path.display()))?;
    set_mode(path, mode)?;
    Ok(())
}

fn set_user_only_permissions(path: &Path) -> Result<()> {
    super::private_fs::set_user_only_file(path)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set user-only permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _mode: u32) -> Result<()> {
    super::private_fs::set_user_only_file(path)
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::metadata(path)
        .with_context(|| format!("failed to read file mode for {}", path.display()))?
        .permissions()
        .mode()
        & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Result<u32> {
    Ok(0o600)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::ensure_observability_bearer;
    use crate::local::LocalPaths;

    #[test]
    fn observability_bearer_is_0600_reused_and_only_rotated_explicitly() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        paths.ensure_persistent_dirs().unwrap();

        assert!(ensure_observability_bearer(&paths, false).unwrap());
        let path = paths.observability_bearer_file();
        let first = std::fs::read_to_string(&path).unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!ensure_observability_bearer(&paths, false).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert!(ensure_observability_bearer(&paths, true).unwrap());
        assert_ne!(std::fs::read_to_string(&path).unwrap(), first);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
