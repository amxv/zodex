use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::ManagedTunnelClientRelease;

const OFFICIAL_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/openai/tunnel-client/releases/latest";
const MAX_CHECKSUM_BYTES: u64 = 1024 * 1024;
const MAX_PLATFORM_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelArchitecture {
    DarwinArm64,
    DarwinAmd64,
    WindowsArm64,
    WindowsAmd64,
}

impl TunnelArchitecture {
    pub fn current_macos() -> Result<Self> {
        match std::env::consts::ARCH {
            "aarch64" => Ok(Self::DarwinArm64),
            "x86_64" => Ok(Self::DarwinAmd64),
            arch => bail!("unsupported macOS architecture for tunnel-client: {arch}"),
        }
    }

    pub fn current_windows() -> Result<Self> {
        match std::env::consts::ARCH {
            "aarch64" => Ok(Self::WindowsArm64),
            "x86_64" => Ok(Self::WindowsAmd64),
            arch => bail!("unsupported Windows architecture for tunnel-client: {arch}"),
        }
    }

    fn asset_suffix(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "-darwin-arm64.zip",
            Self::DarwinAmd64 => "-darwin-amd64.zip",
            Self::WindowsArm64 => "-windows-arm64.zip",
            Self::WindowsAmd64 => "-windows-amd64.zip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTunnelRelease {
    pub version: String,
    pub asset_name: String,
    pub archive_url: String,
    pub archive_sha256: String,
}

impl ResolvedTunnelRelease {
    pub fn managed_metadata(
        &self,
        binary_sha256: String,
        cloudflared_sha256: String,
        cloudflared_manifest_sha256: String,
    ) -> ManagedTunnelClientRelease {
        ManagedTunnelClientRelease {
            version: self.version.clone(),
            asset_name: self.asset_name.clone(),
            archive_sha256: self.archive_sha256.clone(),
            binary_sha256,
            cloudflared_sha256,
            cloudflared_manifest_sha256,
            source_url: self.archive_url.clone(),
        }
    }
}

#[derive(Clone)]
pub struct OfficialTunnelReleaseClient {
    client: Client,
    latest_release_url: String,
}

impl OfficialTunnelReleaseClient {
    pub fn new() -> Result<Self> {
        Self::with_latest_release_url(OFFICIAL_LATEST_RELEASE_API)
    }

    pub fn with_latest_release_url(url: impl Into<String>) -> Result<Self> {
        crate::install_rustls_crypto_provider();
        let client = Client::builder()
            .user_agent(format!("zodex/{} local-setup", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to build tunnel-client release HTTP client")?;
        Ok(Self {
            client,
            latest_release_url: url.into(),
        })
    }

    pub async fn resolve_latest(&self, arch: TunnelArchitecture) -> Result<ResolvedTunnelRelease> {
        let release: GithubRelease = self
            .client
            .get(&self.latest_release_url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("failed to resolve the latest official tunnel-client release")?
            .error_for_status()
            .context("latest official tunnel-client release request failed")?
            .json()
            .await
            .context("failed to parse latest official tunnel-client release metadata")?;

        if release.draft || release.prerelease {
            bail!(
                "official latest tunnel-client release unexpectedly resolved to a draft/prerelease: {}",
                release.tag_name
            );
        }
        if release.tag_name.trim().is_empty() {
            bail!("official tunnel-client release is missing a tag name");
        }

        let expected_name = format!("tunnel-client-{}{}", release.tag_name, arch.asset_suffix());
        let mut matching_assets = release
            .assets
            .iter()
            .filter(|asset| asset.name == expected_name);
        let archive = matching_assets.next().with_context(|| {
            format!(
                "official tunnel-client release {} has no {} asset",
                release.tag_name, expected_name
            )
        })?;
        if matching_assets.next().is_some() {
            bail!(
                "official tunnel-client release {} contains multiple {} assets",
                release.tag_name,
                expected_name
            );
        }
        if archive.size == 0 || archive.size > MAX_PLATFORM_ARCHIVE_BYTES {
            bail!(
                "official tunnel-client asset {} has unexpected size {} bytes",
                archive.name,
                archive.size
            );
        }

        let checksum_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == "SHA256SUMS.txt")
            .context("official tunnel-client release is missing SHA256SUMS.txt")?;
        if checksum_asset.size == 0 || checksum_asset.size > MAX_CHECKSUM_BYTES {
            bail!("official tunnel-client checksum manifest has unexpected size");
        }
        let checksum_text = self
            .download_text(&checksum_asset.browser_download_url, MAX_CHECKSUM_BYTES)
            .await
            .context("failed to download official tunnel-client checksum manifest")?;
        let archive_sha256 = checksum_for_asset(&checksum_text, &archive.name)?;

        Ok(ResolvedTunnelRelease {
            version: release.tag_name,
            asset_name: archive.name.clone(),
            archive_url: archive.browser_download_url.clone(),
            archive_sha256,
        })
    }

    pub async fn download_verified_archive(
        &self,
        release: &ResolvedTunnelRelease,
    ) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(&release.archive_url)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to download official tunnel-client asset {}",
                    release.asset_name
                )
            })?
            .error_for_status()
            .with_context(|| {
                format!(
                    "official tunnel-client asset request failed: {}",
                    release.asset_name
                )
            })?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PLATFORM_ARCHIVE_BYTES)
        {
            bail!("official tunnel-client archive exceeds the setup size limit");
        }
        let bytes = response
            .bytes()
            .await
            .context("failed to read official tunnel-client archive")?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_PLATFORM_ARCHIVE_BYTES {
            bail!("official tunnel-client archive has unexpected size");
        }
        let actual = sha256_hex(&bytes);
        if actual != release.archive_sha256 {
            bail!(
                "official tunnel-client checksum mismatch for {} (expected {}, got {})",
                release.asset_name,
                release.archive_sha256,
                actual
            );
        }
        Ok(bytes.to_vec())
    }

    async fn download_text(&self, url: &str, max_bytes: u64) -> Result<String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("release metadata download failed")?
            .error_for_status()
            .context("release metadata request returned an error status")?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            bail!("release metadata response exceeds the setup size limit");
        }
        let bytes = response
            .bytes()
            .await
            .context("failed to read release metadata")?;
        if bytes.len() as u64 > max_bytes {
            bail!("release metadata response exceeds the setup size limit");
        }
        String::from_utf8(bytes.to_vec()).context("release metadata was not valid UTF-8")
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub fn validate_tunnel_id(value: &str) -> Result<()> {
    let suffix = value
        .strip_prefix("tunnel_")
        .context("tunnel ID must start with `tunnel_`")?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("tunnel ID must be `tunnel_` followed by 32 lowercase hexadecimal characters");
    }
    Ok(())
}

fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String> {
    let mut found = None;
    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        let name = name.trim_start_matches('*');
        if name != asset_name {
            continue;
        }
        if found.is_some() {
            bail!("checksum manifest contains duplicate entries for {asset_name}");
        }
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("checksum manifest contains an invalid SHA-256 for {asset_name}");
        }
        found = Some(hash.to_ascii_lowercase());
    }
    found.ok_or_else(|| anyhow!("checksum manifest has no entry for {asset_name}"))
}

#[cfg(test)]
mod tests {
    use super::{TunnelArchitecture, checksum_for_asset, sha256_hex, validate_tunnel_id};

    #[test]
    fn tunnel_id_validation_matches_current_provider_contract() {
        validate_tunnel_id("tunnel_0123456789abcdef0123456789abcdef").unwrap();
        for invalid in [
            "",
            "tunnel_1234",
            "tunnel_0123456789ABCDEF0123456789ABCDEF",
            "other_0123456789abcdef0123456789abcdef",
            "tunnel_0123456789abcdef0123456789abcdeg",
        ] {
            assert!(
                validate_tunnel_id(invalid).is_err(),
                "{invalid} should fail"
            );
        }
    }

    #[test]
    fn checksum_parser_requires_one_exact_valid_entry() {
        let manifest = format!(
            "{}  other.zip\n{} *wanted.zip\n",
            "a".repeat(64),
            "B".repeat(64)
        );
        assert_eq!(
            checksum_for_asset(&manifest, "wanted.zip").unwrap(),
            "b".repeat(64)
        );
        assert!(checksum_for_asset(&manifest, "missing.zip").is_err());
        assert!(checksum_for_asset("not-a-hash  wanted.zip\n", "wanted.zip").is_err());
    }

    #[test]
    fn architecture_selectors_track_platform_asset_suffixes() {
        assert_eq!(
            TunnelArchitecture::DarwinArm64.asset_suffix(),
            "-darwin-arm64.zip"
        );
        assert_eq!(
            TunnelArchitecture::DarwinAmd64.asset_suffix(),
            "-darwin-amd64.zip"
        );
        assert_eq!(
            TunnelArchitecture::WindowsArm64.asset_suffix(),
            "-windows-arm64.zip"
        );
        assert_eq!(
            TunnelArchitecture::WindowsAmd64.asset_suffix(),
            "-windows-amd64.zip"
        );
    }

    #[test]
    fn sha256_helper_is_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
