use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::AsyncReadExt as _;

const MAX_URL_BYTES: usize = 512;
const MAX_HEALTH_BYTES: usize = 16 * 1024;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

type Sample = std::result::Result<HealthComponent, &'static str>;

#[derive(Default)]
pub(super) struct TunnelDiagnostics {
    last_written: Option<(String, Instant)>,
}

impl TunnelDiagnostics {
    pub(super) async fn sample(&mut self, health_url_file: &Path, ready: bool) -> Option<String> {
        crate::install_rustls_crypto_provider();
        let (poll, delivery) =
            match tokio::time::timeout(HEALTH_TIMEOUT, read_health_url(health_url_file)).await {
                Ok(Ok(base)) => match Client::builder()
                    .no_proxy()
                    .redirect(Policy::none())
                    .timeout(HEALTH_TIMEOUT)
                    .build()
                {
                    Ok(client) => tokio::join!(
                        health(&client, &base, "control-plane"),
                        health(&client, &base, "response-delivery")
                    ),
                    Err(_) => (
                        Err("http_client_unavailable"),
                        Err("http_client_unavailable"),
                    ),
                },
                Ok(Err(reason)) => (Err(reason), Err(reason)),
                Err(_) => (Err("url_file_timeout"), Err("url_file_timeout")),
            };
        let now = OffsetDateTime::now_utc();
        let poll_age = poll
            .as_ref()
            .ok()
            .and_then(|sample| age(sample.details.last_success.as_deref(), now));
        let poll_freshness = match (poll.as_ref(), poll_age) {
            (Ok(sample), Some(age))
                if age as f64 > sample.details.deadline_seconds.clamp(30.0, 180.0) + 15.0 =>
            {
                "stale"
            }
            (Ok(_), Some(_)) => "recent",
            (Ok(_), None) => "not_observed",
            (Err(_), _) => "unavailable",
        };
        let poll_status = classification(&poll);
        let delivery_status = classification(&delivery);
        let key = format!("{ready}:{poll_status}:{poll_freshness}:{delivery_status}");
        let instant = Instant::now();
        if self
            .last_written
            .as_ref()
            .is_some_and(|(previous, written)| {
                previous == &key && instant.duration_since(*written) < SAMPLE_INTERVAL
            })
        {
            return None;
        }
        self.last_written = Some((key, instant));
        let line = serde_json::json!({
            "at": now.format(&Rfc3339).ok(),
            "local_readiness_probe": ready,
            "poll_status": poll_status,
            "poll_freshness": poll_freshness,
            "poll_last_success_age_seconds": poll_age,
            "poll_last_attempt_age_seconds": poll.as_ref().ok().and_then(|sample| age(sample.details.last_attempt.as_deref(), now)),
            "poll_last_error_age_seconds": poll.as_ref().ok().and_then(|sample| age(sample.details.last_error.as_deref(), now)),
            "poll_retry_in_seconds": poll.as_ref().ok().and_then(|sample| seconds_until(sample.details.next_retry.as_deref(), now)),
            "poll_consecutive_failures": poll.as_ref().ok().map(|sample| sample.details.consecutive_failures),
            "poll_http_status": poll.as_ref().ok().and_then(|sample| http_status(sample.details.http_status)),
            "gateway_upload_status": delivery_status,
            "gateway_last_accepted_age_seconds": delivery.as_ref().ok().and_then(|sample| age(sample.details.last_accepted.as_deref(), now)),
            "gateway_upload_accepted_count": delivery.as_ref().ok().map(|sample| sample.details.accepted),
            "gateway_upload_completed_count": delivery.as_ref().ok().map(|sample| sample.details.completed),
            "gateway_upload_retries": delivery.as_ref().ok().map(|sample| sample.details.retries),
            "gateway_upload_disposition": delivery.as_ref().ok().map(|sample| match sample.details.disposition.as_str() {
                "accepted" => "accepted",
                "already_fulfilled_or_unknown" => "already_fulfilled_or_unknown",
                "failed" => "failed",
                "canceled" => "canceled",
                _ => "unknown",
            }),
            "gateway_upload_terminal_failures": delivery.as_ref().ok().map(|sample| sample.details.terminal_failures),
            "gateway_upload_http_status": delivery.as_ref().ok().and_then(|sample| http_status(sample.details.http_status)),
        });
        Some(line.to_string())
    }
}

#[derive(Deserialize)]
struct HealthComponent {
    schema_version: u64,
    component: String,
    status: String,
    details: Details,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Details {
    last_success: Option<String>,
    last_attempt: Option<String>,
    last_error: Option<String>,
    next_retry: Option<String>,
    deadline_seconds: f64,
    consecutive_failures: u64,
    http_status: u16,
    last_accepted: Option<String>,
    accepted: u64,
    completed: u64,
    retries: u64,
    disposition: String,
    terminal_failures: u64,
}

fn classification(sample: &Sample) -> &'static str {
    match sample {
        Ok(sample) => match sample.status.as_str() {
            "ok" => "ok",
            "degraded" => "degraded",
            "disabled" => "disabled",
            _ => "unknown",
        },
        Err(reason) => reason,
    }
}

fn age(timestamp: Option<&str>, now: OffsetDateTime) -> Option<u64> {
    let timestamp = OffsetDateTime::parse(timestamp?, &Rfc3339).ok()?;
    (now - timestamp).whole_seconds().try_into().ok()
}

fn seconds_until(timestamp: Option<&str>, now: OffsetDateTime) -> Option<u64> {
    let timestamp = OffsetDateTime::parse(timestamp?, &Rfc3339).ok()?;
    (timestamp - now).whole_seconds().try_into().ok()
}

fn http_status(code: u16) -> Option<u16> {
    (100..600).contains(&code).then_some(code)
}

async fn read_health_url(path: &Path) -> Result<Url, &'static str> {
    if !tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file())
    {
        return Err("url_file_unavailable");
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "url_file_unavailable")?;
    let mut bytes = Vec::new();
    file.take((MAX_URL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "url_file_unavailable")?;
    if bytes.len() > MAX_URL_BYTES {
        return Err("invalid_health_url");
    }
    let raw = std::str::from_utf8(&bytes).map_err(|_| "invalid_health_url")?;
    parse_health_url(raw.trim()).ok_or("invalid_health_url")
}

fn parse_health_url(raw: &str) -> Option<Url> {
    // Only ever connect to the generated numeric loopback listener. No DNS,
    // proxy, redirects, URL credentials, queries, fragments or arbitrary routes.
    let url = Url::parse(raw).ok()?;
    (url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|port| port > 0)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "/" | "/healthz" | "/readyz")
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

async fn health(client: &Client, base: &Url, component: &str) -> Sample {
    let mut url = base.clone();
    url.set_path(&format!("/health/{component}"));
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "snapshot_unreachable")?;
    if response.status().as_u16() == 404 {
        return Err("snapshot_unsupported");
    }
    if !response.status().is_success() {
        return Err("snapshot_http_error");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "snapshot_unreachable")? {
        if bytes.len().saturating_add(chunk.len()) > MAX_HEALTH_BYTES {
            return Err("snapshot_too_large");
        }
        bytes.extend_from_slice(&chunk);
    }
    let snapshot: HealthComponent =
        serde_json::from_slice(&bytes).map_err(|_| "snapshot_invalid")?;
    if snapshot.schema_version != 1 || snapshot.component != component {
        return Err("snapshot_unsupported");
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_managed_loopback_can_be_sampled() {
        assert!(parse_health_url("http://127.0.0.1:12345").is_some());
        assert!(parse_health_url("http://127.0.0.1:12345/readyz").is_some());
        for invalid in [
            "https://127.0.0.1:12345",
            "http://localhost:12345",
            "http://127.0.0.2:12345",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:12345/?key=secret",
            "http://secret@127.0.0.1:12345/",
            "http://127.0.0.1:12345/other",
        ] {
            assert!(parse_health_url(invalid).is_none(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn samples_distinguish_local_readiness_polling_and_gateway_acceptance() {
        use axum::{Json, Router, routing::get};
        use serde_json::{Value, json};

        let poll = json!({
            "schema_version": 1, "component": "control-plane", "status": "degraded",
            "details": {"last_success": "2020-01-01T00:00:00Z", "deadline_seconds": 35,
                "consecutive_failures": 2, "http_status": 502, "failure_category": "SECRET_VALUE"}
        });
        let delivery = json!({
            "schema_version": 1, "component": "response-delivery", "status": "ok",
            "details": {"accepted": 2, "completed": 3, "retries": 1,
                "disposition": "already_fulfilled_or_unknown", "http_status": 404,
                "response": "SECRET_VALUE"}
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/health/control-plane", get(move || async { Json(poll) }))
            .route(
                "/health/response-delivery",
                get(move || async { Json(delivery) }),
            );
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("generated-url");
        std::fs::write(&file, format!("http://{addr}")).unwrap();
        let mut diagnostics = TunnelDiagnostics::default();
        let line = diagnostics.sample(&file, true).await.unwrap();
        assert!(!line.contains("SECRET_VALUE"));
        let sample: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sample["local_readiness_probe"], true);
        assert_eq!(sample["poll_freshness"], "stale");
        assert_eq!(sample["poll_status"], "degraded");
        assert_eq!(sample["poll_consecutive_failures"], 2);
        assert_eq!(sample["gateway_upload_accepted_count"], 2);
        assert_eq!(sample["gateway_upload_completed_count"], 3);
        assert_eq!(
            sample["gateway_upload_disposition"],
            "already_fulfilled_or_unknown"
        );
        assert!(diagnostics.sample(&file, true).await.is_none());
        assert!(diagnostics.sample(&file, false).await.is_some());
        task.abort();
    }

    #[test]
    fn unknown_server_strings_are_not_logged() {
        let sample = Ok(serde_json::from_str(
            r#"{"schema_version":1,"component":"control-plane","status":"not-a-known-status","details":{"last_success":"not-a-timestamp","http_status":3}}"#,
        ).unwrap());
        assert_eq!(classification(&sample), "unknown");
        let details = &sample.as_ref().unwrap().details;
        assert_eq!(http_status(details.http_status), None);
        assert_eq!(
            age(details.last_success.as_deref(), OffsetDateTime::now_utc()),
            None
        );
    }
}
