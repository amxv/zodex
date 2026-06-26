use std::error::Error as StdError;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::protocol::{
    ApplyPatchInput, ApplyPatchOutput, ExecCommandInput, ToolOutput, WriteStdinInput,
};

pub const ZODEX_URL_ENV: &str = "ZODEX_URL";
pub const ZODEX_KEY_ENV: &str = "ZODEX_KEY";
pub const ZODEX_PROFILE_PATH_ENV: &str = "ZODEX_PROFILE_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionSource {
    Flags,
    Env,
    Profile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConnection {
    pub url: String,
    pub key: String,
    pub source: ConnectionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub url: String,
    pub key: String,
}

#[derive(Debug, Deserialize)]
struct ErrorOutput {
    error: String,
}

pub fn resolve_connection_precedence(
    explicit_url: Option<String>,
    explicit_key: Option<String>,
    env_url: Option<String>,
    env_key: Option<String>,
    profile: Option<ConnectionProfile>,
) -> Result<ResolvedConnection> {
    if explicit_url.is_some() || explicit_key.is_some() {
        let url = explicit_url.ok_or_else(|| anyhow!("missing url in flags"))?;
        let key = explicit_key.ok_or_else(|| anyhow!("missing key in flags"))?;
        return Ok(ResolvedConnection {
            url,
            key,
            source: ConnectionSource::Flags,
        });
    }

    if env_url.is_some() || env_key.is_some() {
        let url = env_url.ok_or_else(|| anyhow!("missing ZODEX_URL in environment"))?;
        let key = env_key.ok_or_else(|| anyhow!("missing ZODEX_KEY in environment"))?;
        return Ok(ResolvedConnection {
            url,
            key,
            source: ConnectionSource::Env,
        });
    }

    let profile = profile.ok_or_else(|| {
        anyhow!(
            "missing connection settings; provide --url/--key, ZODEX_URL/ZODEX_KEY, or run `zodex-client connect`"
        )
    })?;
    Ok(ResolvedConnection {
        url: profile.url,
        key: profile.key,
        source: ConnectionSource::Profile,
    })
}

pub fn resolve_operation_connection(
    explicit_url: Option<String>,
    explicit_key: Option<String>,
    profile_path_override: Option<&Path>,
) -> Result<ResolvedConnection> {
    let profile = load_profile(profile_path_override)?;
    resolve_connection_precedence(
        explicit_url,
        explicit_key,
        std::env::var(ZODEX_URL_ENV).ok(),
        std::env::var(ZODEX_KEY_ENV).ok(),
        profile,
    )
}

pub fn resolve_connect_connection(
    explicit_url: Option<String>,
    explicit_key: Option<String>,
) -> Result<ResolvedConnection> {
    resolve_connection_precedence(
        explicit_url,
        explicit_key,
        std::env::var(ZODEX_URL_ENV).ok(),
        std::env::var(ZODEX_KEY_ENV).ok(),
        None,
    )
}

pub fn profile_path(path_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path_override {
        return Ok(path.to_path_buf());
    }

    if let Ok(path) = std::env::var(ZODEX_PROFILE_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path)
            .join("zodex-client")
            .join("profile.json"));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home)
            .join(".config")
            .join("zodex-client")
            .join("profile.json"));
    }

    bail!(
        "unable to determine profile path; set HOME, XDG_CONFIG_HOME, or {}",
        ZODEX_PROFILE_PATH_ENV
    )
}

pub fn load_profile(path_override: Option<&Path>) -> Result<Option<ConnectionProfile>> {
    let path = profile_path(path_override)?;
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read profile at {}", path.display()))?;
    let profile: ConnectionProfile = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse profile at {}", path.display()))?;
    Ok(Some(profile))
}

pub fn save_profile(profile: &ConnectionProfile, path_override: Option<&Path>) -> Result<PathBuf> {
    let path = profile_path(path_override)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(profile).context("failed to serialize profile")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn delete_profile(path_override: Option<&Path>) -> Result<bool> {
    let path = profile_path(path_override)?;
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    Ok(true)
}

#[derive(Clone)]
pub struct ZodexClient {
    http: reqwest::Client,
    insecure_http: reqwest::Client,
    url: String,
    key: String,
}

impl ZodexClient {
    pub fn new(url: String, key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            insecure_http: reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .expect("insecure reqwest client should build"),
            url,
            key,
        }
    }

    pub async fn exec_command(&self, input: ExecCommandInput) -> Result<ToolOutput> {
        self.post_json("/v1/exec-command", &input).await
    }

    pub async fn write_stdin(&self, input: WriteStdinInput) -> Result<ToolOutput> {
        self.post_json("/v1/write-stdin", &input).await
    }

    pub async fn apply_patch(&self, input: ApplyPatchInput) -> Result<ApplyPatchOutput> {
        self.post_json("/v1/apply-patch", &input).await
    }

    pub async fn verify_connection(&self) -> Result<()> {
        let url = format!("{}/{}", self.url.trim_end_matches('/'), "v1/write-stdin");
        let probe = WriteStdinInput {
            session_handle: "__probe_invalid_handle__".to_string(),
            chars: None,
            yield_time_ms: Some(1),
            kill_process: Some(false),
        };

        let send_request =
            |client: &reqwest::Client| client.post(&url).bearer_auth(&self.key).json(&probe).send();
        let response = match send_request(&self.http).await {
            Ok(response) => response,
            Err(err) if should_retry_insecure(&self.url, &err) => {
                eprintln!(
                    "warning: retrying {url} without TLS certificate verification for a self-signed zodex server"
                );
                send_request(&self.insecure_http)
                    .await
                    .with_context(|| format!("request to {url} failed"))?
            }
            Err(err) => {
                return Err(err).with_context(|| format!("request to {url} failed"));
            }
        };

        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::BAD_REQUEST {
            return Ok(());
        }

        let body = response.text().await.unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<ErrorOutput>(&body) {
            bail!(
                "connection verification failed with status {}: {}",
                status,
                parsed.error
            );
        }
        bail!(
            "connection verification failed with status {}: {}",
            status,
            body.trim()
        );
    }

    async fn post_json<Req, Resp>(&self, path: &str, input: &Req) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        let url = format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        let send_request =
            |client: &reqwest::Client| client.post(&url).bearer_auth(&self.key).json(input).send();

        let response = match send_request(&self.http).await {
            Ok(response) => response,
            Err(err) if should_retry_insecure(&self.url, &err) => {
                eprintln!(
                    "warning: retrying {url} without TLS certificate verification for a self-signed zodex server"
                );
                send_request(&self.insecure_http)
                    .await
                    .with_context(|| format!("request to {url} failed"))?
            }
            Err(err) => {
                return Err(err).with_context(|| format!("request to {url} failed"));
            }
        };

        if response.status().is_success() {
            return response
                .json()
                .await
                .with_context(|| format!("invalid successful response from {url}"));
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<ErrorOutput>(&body) {
            bail!("request failed with status {}: {}", status, parsed.error);
        }
        bail!("request failed with status {}: {}", status, body.trim());
    }
}

fn should_retry_insecure(base_url: &str, error: &reqwest::Error) -> bool {
    is_https_url(base_url) && is_tls_certificate_error(error)
}

fn is_https_url(base_url: &str) -> bool {
    base_url
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("https://")
}

fn is_tls_certificate_error(error: &reqwest::Error) -> bool {
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(err) = current {
        if is_tls_certificate_error_message(&err.to_string()) {
            return true;
        }
        current = err.source();
    }
    false
}

fn is_tls_certificate_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("certificate")
        || lower.contains("cert")
        || lower.contains("unknown issuer")
        || lower.contains("self-signed")
        || lower.contains("self signed")
        || lower.contains("invalid peer certificate")
        || lower.contains("certificate verify failed")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::serve;
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use crate::config::Config;
    use crate::http_api::build_http_api_router;
    use crate::protocol::{CommandStatus, ExecCommandInput};
    use crate::service::ZodexService;

    use super::{
        ConnectionProfile, ConnectionSource, ZodexClient, is_tls_certificate_error_message,
        resolve_connection_precedence, save_profile,
    };

    #[test]
    fn tls_certificate_message_detection_is_narrow() {
        assert!(is_tls_certificate_error_message(
            "invalid peer certificate: UnknownIssuer"
        ));
        assert!(is_tls_certificate_error_message(
            "certificate verify failed"
        ));
        assert!(!is_tls_certificate_error_message(
            "dns error: failed to lookup address information"
        ));
        assert!(!is_tls_certificate_error_message("connection refused"));
    }

    #[tokio::test]
    async fn verify_connection_rejects_unauthorized_target() {
        crate::install_rustls_crypto_provider();

        let api_key = "client-auth-key".to_string();
        let config = Arc::new(Config {
            api_key,
            ..Config::default()
        });
        let service = ZodexService::new(config.clone());
        let app = build_http_api_router(config, service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server should run");
        });

        let client = ZodexClient::new(format!("http://{addr}"), "wrong-key".to_string());
        let err = client
            .verify_connection()
            .await
            .expect_err("verify should fail for invalid key");
        assert!(
            err.to_string().contains("status 401"),
            "unexpected error: {err:?}"
        );

        let _ = shutdown_tx.send(());
        server.await.expect("server join should succeed");
    }

    #[test]
    fn resolution_prefers_flags_then_env_then_profile() {
        let profile = ConnectionProfile {
            url: "http://from-profile".to_string(),
            key: "profile-key".to_string(),
        };

        let from_flags = resolve_connection_precedence(
            Some("http://from-flags".to_string()),
            Some("flags-key".to_string()),
            Some("http://from-env".to_string()),
            Some("env-key".to_string()),
            Some(profile.clone()),
        )
        .expect("flags should win");
        assert_eq!(from_flags.source, ConnectionSource::Flags);
        assert_eq!(from_flags.url, "http://from-flags");
        assert_eq!(from_flags.key, "flags-key");

        let from_env = resolve_connection_precedence(
            None,
            None,
            Some("http://from-env".to_string()),
            Some("env-key".to_string()),
            Some(profile.clone()),
        )
        .expect("env should win when flags absent");
        assert_eq!(from_env.source, ConnectionSource::Env);
        assert_eq!(from_env.url, "http://from-env");
        assert_eq!(from_env.key, "env-key");

        let from_profile =
            resolve_connection_precedence(None, None, None, None, Some(profile.clone()))
                .expect("profile should be used last");
        assert_eq!(from_profile.source, ConnectionSource::Profile);
        assert_eq!(from_profile.url, profile.url);
        assert_eq!(from_profile.key, profile.key);
    }

    #[test]
    fn resolution_requires_complete_pairs_per_source() {
        let err = resolve_connection_precedence(
            Some("http://from-flags".to_string()),
            None,
            None,
            None,
            None,
        )
        .expect_err("missing key should error");
        assert!(err.to_string().contains("missing key in flags"));

        let err = resolve_connection_precedence(
            None,
            None,
            Some("http://from-env".to_string()),
            None,
            None,
        )
        .expect_err("missing env key should error");
        assert!(err.to_string().contains("missing ZODEX_KEY in environment"));

        let err = resolve_connection_precedence(None, None, None, None, None)
            .expect_err("missing all sources should error");
        assert!(err.to_string().contains("zodex-client connect"));
    }

    #[test]
    fn connect_disconnect_profile_persistence_round_trip() {
        let dir = tempdir().expect("tempdir");
        let profile_path = dir.path().join("zodex-client-profile.json");
        let profile = ConnectionProfile {
            url: "http://saved-profile".to_string(),
            key: "saved-key".to_string(),
        };

        let written_path = save_profile(&profile, Some(&profile_path)).expect("save profile");
        assert_eq!(written_path, profile_path);
        let loaded = super::load_profile(Some(&profile_path))
            .expect("load profile")
            .expect("profile should exist");
        assert_eq!(loaded, profile);

        let removed = super::delete_profile(Some(&profile_path)).expect("delete profile");
        assert!(removed);
        assert!(
            super::load_profile(Some(&profile_path))
                .expect("load profile after delete")
                .is_none()
        );
    }

    #[tokio::test]
    async fn client_exec_command_smoke_round_trip_against_http_api() {
        crate::install_rustls_crypto_provider();

        let api_key = "client-smoke-key".to_string();
        let config = Arc::new(Config {
            api_key: api_key.clone(),
            ..Config::default()
        });
        let service = ZodexService::new(config.clone());
        let app = build_http_api_router(config, service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server should run");
        });

        let client = ZodexClient::new(format!("http://{addr}"), api_key);
        let output = client
            .exec_command(ExecCommandInput {
                cmd: "echo client-smoke".to_string(),
                yield_time_ms: Some(2_000),
                workdir: None,
                timeout_ms: None,
            })
            .await
            .expect("exec command should succeed");

        assert_eq!(output.status, CommandStatus::Exited);
        assert!(output.output.contains("client-smoke"));

        let _ = shutdown_tx.send(());
        server.await.expect("server join should succeed");
    }
}
