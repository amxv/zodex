use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/zodex/config.toml";

const MIN_YIELD_MS: u64 = 50;
const MAX_YIELD_MS: u64 = 60_000;
const MIN_EXEC_TIMEOUT_MS: u64 = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind_host: String,
    pub bind_port: u16,
    pub http_bind_port: Option<u16>,
    pub api_key: String,
    pub tls_mode: String,
    pub tls_cert_path: String,
    pub tls_key_path: String,
    pub max_sessions: usize,
    pub default_exec_timeout_ms: u64,
    pub max_exec_timeout_ms: u64,
    pub default_exec_yield_time_ms: u64,
    pub default_write_yield_time_ms: u64,
    pub max_output_chars: usize,
    pub reader_app_id: Option<u64>,
    pub reader_installation_id: Option<u64>,
    pub reader_private_key_path: String,
    pub publisher_socket_path: String,
    pub publisher_private_key_path: String,
    pub publisher_app_id: Option<u64>,
    pub publisher_client_id: Option<String>,
    pub agent_user: String,
    pub agent_home: String,
    pub default_workdir: String,
    pub publisher_user: String,
    pub service_group: String,
    pub publisher_branch_prefix: String,
    pub publisher_max_bundle_bytes: usize,
    pub publisher_max_title_chars: usize,
    pub publisher_max_body_chars: usize,
    pub publisher_installations: Vec<PublisherInstallation>,
    pub publisher_targets: Vec<PublishTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PublisherInstallation {
    pub account: String,
    pub installation_id: u64,
    pub default_base: String,
}

impl Default for PublisherInstallation {
    fn default() -> Self {
        Self {
            account: String::new(),
            installation_id: 0,
            default_base: "main".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PublishTarget {
    pub id: String,
    pub repo: String,
    pub default_base: String,
    pub installation_id: u64,
}

impl Default for PublishTarget {
    fn default() -> Self {
        Self {
            id: String::new(),
            repo: String::new(),
            default_base: "main".to_string(),
            installation_id: 0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_host: "0.0.0.0".to_string(),
            bind_port: 443,
            http_bind_port: None,
            api_key: "change-me".to_string(),
            tls_mode: "auto".to_string(),
            tls_cert_path: "/var/lib/zodex/tls/cert.pem".to_string(),
            tls_key_path: "/var/lib/zodex/tls/key.pem".to_string(),
            max_sessions: 64,
            default_exec_timeout_ms: 7_200_000,
            max_exec_timeout_ms: 7_200_000,
            default_exec_yield_time_ms: 10_000,
            default_write_yield_time_ms: 10_000,
            max_output_chars: 200_000,
            reader_app_id: None,
            reader_installation_id: None,
            reader_private_key_path: "/etc/zodex/reader/private-key.pem".to_string(),
            publisher_socket_path: "/var/lib/zodex/publisher/run/zodex-prd.sock".to_string(),
            publisher_private_key_path: "/etc/zodex/publisher/private-key.pem".to_string(),
            publisher_app_id: None,
            publisher_client_id: None,
            agent_user: "zodex-agent".to_string(),
            agent_home: "/home/zodex-agent".to_string(),
            default_workdir: "/workspace".to_string(),
            publisher_user: "zodex-publisher".to_string(),
            service_group: "zodex".to_string(),
            publisher_branch_prefix: "agent".to_string(),
            publisher_max_bundle_bytes: 8 * 1024 * 1024,
            publisher_max_title_chars: 240,
            publisher_max_body_chars: 16_000,
            publisher_installations: Vec::new(),
            publisher_targets: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));

        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let parsed = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }

        let raw = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(path, raw)
            .with_context(|| format!("failed to write config at {}", path.display()))?;
        Ok(())
    }

    pub fn clamp_exec_yield_ms(&self, requested: Option<u64>) -> u64 {
        let raw = requested.unwrap_or(self.default_exec_yield_time_ms);
        raw.clamp(MIN_YIELD_MS, MAX_YIELD_MS)
    }

    pub fn clamp_write_yield_ms(&self, requested: Option<u64>) -> u64 {
        let raw = requested.unwrap_or(self.default_write_yield_time_ms);
        raw.clamp(MIN_YIELD_MS, MAX_YIELD_MS)
    }

    pub fn clamp_exec_timeout_ms(&self, requested: Option<u64>) -> u64 {
        let raw = requested.unwrap_or(self.default_exec_timeout_ms);
        raw.clamp(MIN_EXEC_TIMEOUT_MS, self.max_exec_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, PublishTarget, PublisherInstallation};

    #[test]
    fn clamp_yields_and_timeout() {
        let cfg = Config::default();

        assert_eq!(cfg.clamp_exec_yield_ms(Some(1)), 50);
        assert_eq!(cfg.clamp_write_yield_ms(Some(100_000)), 60_000);
        assert_eq!(cfg.clamp_exec_timeout_ms(Some(1)), 1_000);
        assert_eq!(cfg.clamp_exec_timeout_ms(Some(9_000_000)), 7_200_000);
    }

    #[test]
    fn config_defaults_include_publisher_settings() {
        let cfg = Config::default();

        assert_eq!(cfg.http_bind_port, None);
        assert_eq!(cfg.reader_app_id, None);
        assert_eq!(cfg.reader_installation_id, None);
        assert!(
            cfg.reader_private_key_path
                .ends_with("/reader/private-key.pem")
        );
        assert!(cfg.publisher_socket_path.ends_with("zodex-prd.sock"));
        assert!(cfg.publisher_private_key_path.ends_with("private-key.pem"));
        assert_eq!(cfg.agent_user, "zodex-agent");
        assert_eq!(cfg.agent_home, "/home/zodex-agent");
        assert_eq!(cfg.default_workdir, "/workspace");
        assert_eq!(cfg.publisher_client_id, None);
        assert_eq!(cfg.publisher_user, "zodex-publisher");
        assert_eq!(cfg.service_group, "zodex");
        assert_eq!(cfg.publisher_branch_prefix, "agent");
        assert_eq!(cfg.publisher_max_bundle_bytes, 8 * 1024 * 1024);
        assert!(cfg.publisher_installations.is_empty());
        assert!(cfg.publisher_targets.is_empty());
    }

    #[test]
    fn missing_publisher_fields_are_backfilled_from_defaults() {
        let parsed: Config = toml::from_str(
            r#"
bind_host = "0.0.0.0"
bind_port = 443
http_bind_port = 8080
api_key = "test"
tls_mode = "auto"
tls_cert_path = "/tmp/cert.pem"
tls_key_path = "/tmp/key.pem"
max_sessions = 64
default_exec_timeout_ms = 1000
max_exec_timeout_ms = 2000
default_exec_yield_time_ms = 10000
default_write_yield_time_ms = 10000
max_output_chars = 200000
"#,
        )
        .expect("config should parse");

        assert_eq!(parsed.reader_app_id, None);
        assert_eq!(parsed.reader_installation_id, None);
        assert_eq!(parsed.http_bind_port, Some(8080));
        assert_eq!(
            parsed.reader_private_key_path,
            "/etc/zodex/reader/private-key.pem"
        );
        assert_eq!(parsed.publisher_app_id, None);
        assert_eq!(parsed.publisher_client_id, None);
        assert_eq!(parsed.agent_user, "zodex-agent");
        assert_eq!(parsed.agent_home, "/home/zodex-agent");
        assert_eq!(parsed.default_workdir, "/workspace");
        assert_eq!(parsed.publisher_user, "zodex-publisher");
        assert_eq!(parsed.service_group, "zodex");
        assert_eq!(parsed.publisher_branch_prefix, "agent");
        assert!(parsed.publisher_installations.is_empty());
        assert!(parsed.publisher_targets.is_empty());
    }

    #[test]
    fn publisher_installation_defaults_to_main_base() {
        let installation: PublisherInstallation = toml::from_str(
            r#"
account = "amxv"
installation_id = 123
"#,
        )
        .expect("publisher installation should parse");

        assert_eq!(installation.default_base, "main");
    }

    #[test]
    fn publish_target_defaults_to_main_base() {
        let target: PublishTarget = toml::from_str(
            r#"
id = "amxv/zodex"
repo = "amxv/zodex"
installation_id = 123
"#,
        )
        .expect("publish target should parse");

        assert_eq!(target.default_base, "main");
    }
}
