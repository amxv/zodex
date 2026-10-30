use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::{HumanDuration, StorageSize};

const DEFAULT_HISTORY_MAX_AGE_SECONDS: u64 = 60 * 24 * 60 * 60;
const DEFAULT_HISTORY_MAX_SIZE_BYTES: u64 = 500 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub tunnel: LocalTunnelConfig,
    pub history: LocalHistoryConfig,
    pub context: LocalContextConfig,
    pub liveboard: LocalLiveboardConfig,
}

/// Explicitly trusted HTTPS browser origins for a Liveboard behind a reverse proxy.
/// Without any entries, the Liveboard only accepts its own loopback Host/Origin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LocalLiveboardConfig {
    pub trusted_origins: Vec<String>,
}

pub(crate) fn parse_trusted_liveboard_origin(value: &str) -> Result<Url> {
    let parsed = Url::parse(value).with_context(|| {
        format!("invalid liveboard.trusted_origins entry {value:?}: expected an HTTPS origin")
    })?;
    // The canonical serialized origin has no path, query, fragment, userinfo,
    // or wildcards. Requiring an exact match prevents ambiguous host matching.
    if parsed.scheme() != "https"
        || parsed.host_str().is_none_or(|host| host.contains('*'))
        || parsed.port() == Some(0)
        || parsed.origin().ascii_serialization() != value
    {
        bail!(
            "invalid liveboard.trusted_origins entry {value:?}: use an exact HTTPS origin such as https://liveboard.example.com (no path, wildcard, or credentials)"
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LocalContextConfig {
    pub enabled: bool,
    pub global_agents: bool,
    pub repo_agents: bool,
    pub repo_skills: bool,
    pub skills: LocalContextSkillsConfig,
}

impl Default for LocalContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            global_agents: true,
            repo_agents: true,
            repo_skills: true,
            skills: LocalContextSkillsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LocalContextSkillsConfig {
    pub enabled: bool,
    pub agents: bool,
    pub codex: bool,
    pub paths: Vec<PathBuf>,
}

impl Default for LocalContextSkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: true,
            codex: true,
            paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LocalTunnelConfig {
    pub id: Option<String>,
    pub client_path: Option<PathBuf>,
    pub release: Option<ManagedTunnelClientRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedTunnelClientRelease {
    pub version: String,
    pub asset_name: String,
    pub archive_sha256: String,
    pub binary_sha256: String,
    pub cloudflared_sha256: String,
    pub cloudflared_manifest_sha256: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LocalHistoryConfig {
    pub max_age: HumanDuration,
    pub max_size: StorageSize,
}

impl Default for LocalHistoryConfig {
    fn default() -> Self {
        Self {
            max_age: HumanDuration::from_seconds(DEFAULT_HISTORY_MAX_AGE_SECONDS),
            max_size: StorageSize::from_bytes(DEFAULT_HISTORY_MAX_SIZE_BYTES),
        }
    }
}

impl LocalConfig {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read Local config at {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse Local config at {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .context("Local config path must have a parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Local config directory {}",
                parent.display()
            )
        })?;
        let raw = toml::to_string_pretty(self).context("failed to serialize Local config")?;
        let mut temp = NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "failed to create temporary Local config in {}",
                parent.display()
            )
        })?;
        use std::io::Write as _;
        temp.write_all(raw.as_bytes())
            .context("failed to write temporary Local config")?;
        temp.as_file()
            .sync_all()
            .context("failed to sync temporary Local config")?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to persist Local config at {}", path.display()))?;
        Ok(())
    }

    pub fn is_provider_configured(&self) -> bool {
        self.tunnel
            .id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.tunnel.client_path.is_some()
            && self.tunnel.release.is_some()
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "history.max-age" => self.history.max_age = value.parse()?,
            "history.max-size" => self.history.max_size = value.parse()?,
            "context.enabled" => self.context.enabled = parse_bool(value)?,
            "context.global-agents" => self.context.global_agents = parse_bool(value)?,
            "context.repo-agents" => self.context.repo_agents = parse_bool(value)?,
            "context.repo-skills" => self.context.repo_skills = parse_bool(value)?,
            "context.skills.enabled" => self.context.skills.enabled = parse_bool(value)?,
            "context.skills.agents" => self.context.skills.agents = parse_bool(value)?,
            "context.skills.codex" => self.context.skills.codex = parse_bool(value)?,
            "context.skills.paths" => self.context.skills.paths = parse_paths(value)?,
            "liveboard.trusted-origins" => {
                self.liveboard.trusted_origins = parse_trusted_origins(value)?
            }
            "tunnel.id" => {
                let value = value.trim();
                super::validate_tunnel_id(value)?;
                self.tunnel.id = Some(value.to_owned());
            }
            _ => bail!("unknown Local config key `{key}`"),
        }
        Ok(())
    }

    pub fn can_set_while_running(key: &str) -> bool {
        key == "context.enabled"
            || key == "context.global-agents"
            || key == "context.repo-agents"
            || key == "context.repo-skills"
            || key.starts_with("context.skills.")
    }

    pub fn get(&self, key: &str) -> Result<String> {
        match key {
            "history.max-age" => Ok(self.history.max_age.to_string()),
            "history.max-size" => Ok(self.history.max_size.to_string()),
            "context.enabled" => Ok(self.context.enabled.to_string()),
            "context.global-agents" => Ok(self.context.global_agents.to_string()),
            "context.repo-agents" => Ok(self.context.repo_agents.to_string()),
            "context.repo-skills" => Ok(self.context.repo_skills.to_string()),
            "context.skills.enabled" => Ok(self.context.skills.enabled.to_string()),
            "context.skills.agents" => Ok(self.context.skills.agents.to_string()),
            "context.skills.codex" => Ok(self.context.skills.codex.to_string()),
            "context.skills.paths" => Ok(render_paths(&self.context.skills.paths)),
            "liveboard.trusted-origins" => Ok(render_strings(&self.liveboard.trusted_origins)),
            "tunnel.id" => Ok(self
                .tunnel
                .id
                .clone()
                .unwrap_or_else(|| "unset".to_string())),
            "tunnel.client-path" => Ok(self
                .tunnel
                .client_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unset".to_string())),
            _ => bail!("unknown Local config key `{key}`"),
        }
    }

    fn validate(&self) -> Result<()> {
        for path in &self.context.skills.paths {
            let rendered = path.to_string_lossy();
            if !path.is_absolute() && rendered != "~" && !rendered.starts_with("~/") {
                bail!(
                    "context.skills.paths entries must be absolute or start with `~/`: {}",
                    path.display()
                );
            }
        }
        for origin in &self.liveboard.trusted_origins {
            parse_trusted_liveboard_origin(origin)?;
        }
        Ok(())
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" => Ok(true),
        "false" | "off" | "0" => Ok(false),
        _ => bail!("expected boolean value true/false, on/off, or 1/0"),
    }
}

fn parse_paths(value: &str) -> Result<Vec<PathBuf>> {
    let source = format!("paths = {}", value.trim());
    let parsed: toml::Value =
        toml::from_str(&source).context("context.skills.paths must be a TOML array of strings")?;
    let values = parsed
        .get("paths")
        .and_then(toml::Value::as_array)
        .context("context.skills.paths must be a TOML array of strings")?;
    let paths = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .context("context.skills.paths entries must be strings")
        })
        .collect::<Result<Vec<_>>>()?;
    for path in &paths {
        let rendered = path.to_string_lossy();
        if !path.is_absolute() && rendered != "~" && !rendered.starts_with("~/") {
            bail!(
                "context.skills.paths entries must be absolute or start with `~/`: {}",
                path.display()
            );
        }
    }
    Ok(paths)
}

fn render_paths(paths: &[PathBuf]) -> String {
    toml::Value::Array(
        paths
            .iter()
            .map(|path| toml::Value::String(path.display().to_string()))
            .collect(),
    )
    .to_string()
}

fn parse_trusted_origins(value: &str) -> Result<Vec<String>> {
    let source = format!("origins = {}", value.trim());
    let parsed: toml::Value = toml::from_str(&source)
        .context("liveboard.trusted-origins must be a TOML array of HTTPS origin strings")?;
    let entries = parsed
        .get("origins")
        .and_then(toml::Value::as_array)
        .context("liveboard.trusted-origins must be a TOML array of HTTPS origin strings")?;
    entries
        .iter()
        .map(|entry| {
            let origin = entry
                .as_str()
                .context("liveboard.trusted-origins entries must be strings")?;
            parse_trusted_liveboard_origin(origin)?;
            Ok(origin.to_owned())
        })
        .collect()
}

fn render_strings(strings: &[String]) -> String {
    toml::Value::Array(strings.iter().cloned().map(toml::Value::String).collect()).to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::LocalConfig;

    #[test]
    fn local_config_defaults_and_round_trip_are_human_readable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        assert_eq!(config.get("history.max-age").unwrap(), "60d");
        assert_eq!(config.get("history.max-size").unwrap(), "500mb");
        assert_eq!(config.get("context.enabled").unwrap(), "true");
        assert_eq!(config.get("context.global-agents").unwrap(), "true");
        assert_eq!(config.get("context.repo-agents").unwrap(), "true");
        assert_eq!(config.get("context.repo-skills").unwrap(), "true");
        assert_eq!(config.get("context.skills.enabled").unwrap(), "true");
        assert_eq!(config.get("context.skills.agents").unwrap(), "true");
        assert_eq!(config.get("context.skills.codex").unwrap(), "true");
        assert_eq!(config.get("context.skills.paths").unwrap(), "[]");
        assert_eq!(config.get("liveboard.trusted-origins").unwrap(), "[]");
        assert!(!config.is_provider_configured());

        config.set("history.max-age", "2d").unwrap();
        config.set("history.max-size", "1gb").unwrap();
        config.set("context.repo-agents", "off").unwrap();
        config.set("context.repo-skills", "off").unwrap();
        config
            .set(
                "context.skills.paths",
                r#"["/opt/team-skills", "/Users/example/skills"]"#,
            )
            .unwrap();
        config
            .set("tunnel.id", "tunnel_0123456789abcdef0123456789abcdef")
            .unwrap();
        config
            .set(
                "liveboard.trusted-origins",
                r#"["https://liveboard.ashray.xyz", "https://example.com:8443"]"#,
            )
            .unwrap();
        config.tunnel.client_path = Some(dir.path().join("tunnel-client"));
        config.tunnel.release = Some(super::ManagedTunnelClientRelease {
            version: "v0.0.11".to_string(),
            asset_name: "tunnel-client-v0.0.11-darwin-arm64.zip".to_string(),
            archive_sha256: "a".repeat(64),
            binary_sha256: "b".repeat(64),
            cloudflared_sha256: "c".repeat(64),
            cloudflared_manifest_sha256: "d".repeat(64),
            source_url: "https://example.invalid/archive.zip".to_string(),
        });
        config.save(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("max_age = \"2d\""));
        assert!(raw.contains("max_size = \"1gb\""));
        assert!(raw.contains("repo_agents = false"));
        assert!(raw.contains("repo_skills = false"));
        assert!(raw.contains("/opt/team-skills"));
        assert!(raw.contains("[liveboard]"));
        assert!(raw.contains("https://liveboard.ashray.xyz"));
        assert!(!raw.contains("api_key"));
        assert!(!raw.contains("runtime_key"));
        assert!(config.is_provider_configured());

        let loaded = LocalConfig::load(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn local_config_rejects_invalid_values_and_unknown_fields() {
        let mut config = LocalConfig::default();
        assert!(config.set("history.max-age", "forever").is_err());
        assert!(config.set("history.max-size", "huge").is_err());
        assert!(config.set("context.enabled", "maybe").is_err());
        assert!(config.set("context.skills.paths", "not-an-array").is_err());
        assert!(
            config
                .set("context.skills.paths", r#"["relative/skills"]"#)
                .is_err()
        );
        assert!(config.set("secret.api-key", "do-not-store").is_err());
        assert!(config.set("liveboard.trusted-origins", "hello").is_err());
        assert!(
            config
                .set("liveboard.trusted-origins", r#"["http://example.com"]"#)
                .is_err()
        );
        assert!(
            config
                .set(
                    "liveboard.trusted-origins",
                    r#"["https://example.com/path"]"#
                )
                .is_err()
        );

        let parsed = toml::from_str::<LocalConfig>(
            "[history]\nmax_age = \"60d\"\nmax_size = \"500mb\"\nsecret = \"nope\"\n",
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn legacy_context_config_defaults_repo_skills_on() {
        let config = toml::from_str::<LocalConfig>(
            "[context]\nenabled = true\nglobal_agents = true\nrepo_agents = true\n",
        )
        .unwrap();
        assert!(config.context.repo_skills);
        assert!(config.liveboard.trusted_origins.is_empty());
    }

    #[test]
    fn trusted_liveboard_origins_accept_exact_https_only() {
        for valid in [
            "https://liveboard.ashray.xyz",
            "https://example.com:8443",
            "https://[2001:db8::1]:8443",
        ] {
            super::parse_trusted_liveboard_origin(valid).unwrap();
        }
        for invalid in [
            "",
            "http://liveboard.ashray.xyz",
            "https://example.com/",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
            "https://user:password@example.com",
            "https://*.example.com",
            "https://example.com:443",
            "HTTPS://example.com",
            "https://example.com,https://other.com",
            "https://example.com:0",
            "https://example.com ",
        ] {
            assert!(
                super::parse_trusted_liveboard_origin(invalid).is_err(),
                "unexpectedly allowed {invalid:?}"
            );
        }
        let raw = "[liveboard]\ntrusted_origins = [\"https://example.com\", \"http://insecure.example\"]\n";
        let config: LocalConfig = toml::from_str(raw).unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.toml");
        std::fs::write(&path, raw).unwrap();
        assert!(LocalConfig::load(&path).is_err());
        assert!(config.save(&path).is_err());
    }

    #[test]
    fn only_context_settings_are_live_mutable() {
        for key in [
            "context.enabled",
            "context.global-agents",
            "context.repo-agents",
            "context.repo-skills",
            "context.skills.enabled",
            "context.skills.agents",
            "context.skills.codex",
            "context.skills.paths",
        ] {
            assert!(LocalConfig::can_set_while_running(key), "{key}");
        }
        for key in [
            "history.max-age",
            "history.max-size",
            "tunnel.id",
            "liveboard.trusted-origins",
        ] {
            assert!(!LocalConfig::can_set_while_running(key), "{key}");
        }
    }
}
