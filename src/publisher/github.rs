use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;

use super::api::{
    CreatePullRequestPayload, CreatePullRequestResponse, GithubAppClaims,
    InstallationTokenResponse, MintedInstallationToken, PublishPrResponse,
};
use super::{
    ASKPASS_MODE, ASKPASS_SCRIPT_NAME, DEFAULT_USER_AGENT, GITHUB_API_BASE, GITHUB_API_VERSION,
};

pub(super) fn github_repo_https_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

pub(super) fn clone_repo_with_token(
    parent_dir: &Path,
    token: &str,
    askpass_path: &Path,
    repo: &str,
) -> Result<PathBuf> {
    let repo_dir = parent_dir.join("repo");
    git_with_token(
        parent_dir,
        token,
        askpass_path,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            &github_repo_https_url(repo),
            repo_dir.to_str().unwrap(),
        ],
    )?;
    Ok(repo_dir)
}

pub(super) fn write_askpass_script(dir: &Path) -> Result<PathBuf> {
    let script_path = dir.join(ASKPASS_SCRIPT_NAME);
    let mut file = fs::File::create(&script_path)
        .with_context(|| format!("failed to create {}", script_path.display()))?;
    file.write_all(
        br#"#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' "x-access-token" ;;
  *Password*) printf '%s\n' "${GITHUB_APP_TOKEN}" ;;
  *) printf '\n' ;;
esac
"#,
    )
    .with_context(|| format!("failed to write {}", script_path.display()))?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(ASKPASS_MODE))
        .with_context(|| format!("failed to chmod {}", script_path.display()))?;
    Ok(script_path)
}

pub(super) fn git_plain(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    check_command_output("git", args, output)
}

pub(super) fn git_with_token(
    cwd: &Path,
    token: &str,
    askpass_path: &Path,
    args: &[&str],
) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GITHUB_APP_TOKEN", token)
        .env("GIT_ASKPASS", askpass_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .with_context(|| format!("failed to run authenticated git {}", args.join(" ")))?;
    check_command_output("git", args, output)
}

fn check_command_output(
    program: &str,
    args: &[&str],
    output: std::process::Output,
) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        return Ok(stdout);
    }

    let status = output.status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    bail!(
        "{program} {} failed (status: {status})\n{details}",
        args.join(" ")
    )
}

pub async fn mint_reader_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<String> {
    Ok(mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Reader,
    )
    .await?
    .token)
}

pub async fn mint_publisher_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<String> {
    Ok(mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Publisher,
    )
    .await?
    .token)
}

pub async fn mint_publisher_installation_token_with_metadata(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<MintedInstallationToken> {
    mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Publisher,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenPermissionProfile {
    Reader,
    Publisher,
}

async fn mint_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
    permissions: TokenPermissionProfile,
) -> Result<MintedInstallationToken> {
    let key_pem = fs::read(private_key_path)
        .with_context(|| format!("failed to read {}", private_key_path.display()))?;
    let encoding_key = EncodingKey::from_rsa_pem(&key_pem).with_context(|| {
        format!(
            "failed to parse {} RSA private key",
            permissions.private_key_label()
        )
    })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    let claims = GithubAppClaims {
        iat: now.saturating_sub(60),
        exp: now + 540,
        iss: app_id.to_string(),
    };
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .context("failed to encode GitHub App JWT")?;

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{GITHUB_API_BASE}/app/installations/{installation_id}/access_tokens"
        ))
        .header(ACCEPT, "application/vnd.github+json")
        .header(AUTHORIZATION, format!("Bearer {jwt}"))
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_USER_AGENT)
        .json(&serde_json::json!({ "permissions": permissions.github_permissions() }))
        .send()
        .await
        .context("failed to request GitHub installation token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub installation token request failed ({status}): {body}");
    }

    let payload: InstallationTokenResponse = response
        .json()
        .await
        .context("failed to decode GitHub installation token response")?;
    Ok(MintedInstallationToken {
        token: payload.token,
        expires_at: payload.expires_at,
    })
}

pub async fn resolve_repo_installation_id(
    app_id: u64,
    private_key_path: &Path,
    repo: &str,
) -> Result<u64> {
    #[derive(Debug, Deserialize)]
    struct RepoInstallationResponse {
        id: u64,
    }

    let key_pem = fs::read(private_key_path)
        .with_context(|| format!("failed to read {}", private_key_path.display()))?;
    let encoding_key =
        EncodingKey::from_rsa_pem(&key_pem).context("failed to parse publisher RSA private key")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    let claims = GithubAppClaims {
        iat: now.saturating_sub(60),
        exp: now + 540,
        iss: app_id.to_string(),
    };
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .context("failed to encode GitHub App JWT")?;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{GITHUB_API_BASE}/repos/{repo}/installation"))
        .header(ACCEPT, "application/vnd.github+json")
        .header(AUTHORIZATION, format!("Bearer {jwt}"))
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_USER_AGENT)
        .send()
        .await
        .context("failed to resolve GitHub App installation for repo")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub repo installation lookup failed ({status}): {body}");
    }

    let payload: RepoInstallationResponse = response
        .json()
        .await
        .context("failed to decode GitHub repo installation response")?;
    Ok(payload.id)
}

impl TokenPermissionProfile {
    fn private_key_label(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Publisher => "publisher",
        }
    }

    pub(super) fn github_permissions(self) -> serde_json::Value {
        match self {
            Self::Reader => serde_json::json!({
                "contents": "read"
            }),
            Self::Publisher => serde_json::json!({
                "contents": "write",
                "pull_requests": "write",
                "workflows": "write"
            }),
        }
    }
}

/// Create a GitHub pull request via the REST API using an already-resolved token.
///
/// This is shared by the controlled publisher flow after it pushes a generated
/// branch. It performs a single `POST /repos/{repo}/pulls` call and never shells
/// out to `gh`.
pub async fn create_pull_request(
    token: &str,
    repo: &str,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<PublishPrResponse> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .context("failed to build authorization header")?,
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{GITHUB_API_BASE}/repos/{repo}/pulls"))
        .headers(headers)
        .json(&CreatePullRequestPayload {
            title,
            body,
            head: branch,
            base,
            draft,
        })
        .send()
        .await
        .context("failed to create GitHub pull request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub pull request creation failed ({status}): {body}");
    }

    let payload: CreatePullRequestResponse = response
        .json()
        .await
        .context("failed to decode GitHub pull request response")?;

    Ok(PublishPrResponse {
        pr_url: payload.html_url,
        branch: branch.to_string(),
        pull_number: payload.number,
    })
}
