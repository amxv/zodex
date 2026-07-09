use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config::{Config, PublishTarget};

use super::api::{
    DirectPushRequest, DirectPushResponse, PublishPrError, PublishPrRequest, PublishPrResponse,
    PublisherRequest, PublisherResponse,
};
use super::github::{
    clone_repo_with_token, create_pull_request, git_plain, git_with_token, github_repo_https_url,
    mint_publisher_installation_token, write_askpass_script,
};
use super::validation::{
    build_publish_branch_name, validate_direct_push_request, validate_git_object_id,
    validate_publish_request, validate_publisher_config,
};
use super::{
    DIRECT_PUSH_IMPORTED_REF, IMPORTED_REF, MAX_SOCKET_REQUEST_BYTES, SOCKET_DIR_MODE, SOCKET_MODE,
};

pub(super) fn ensure_publisher_socket_parent_dir(socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create publisher socket directory {}",
                parent.display()
            )
        })?;
        fs::set_permissions(parent, fs::Permissions::from_mode(SOCKET_DIR_MODE))
            .with_context(|| format!("failed to chmod {}", parent.display()))?;
    }

    Ok(())
}

pub async fn serve_publisher(config: Config) -> Result<()> {
    validate_publisher_config(&config)?;

    let socket_path = Path::new(&config.publisher_socket_path);
    ensure_publisher_socket_parent_dir(socket_path)?;

    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove stale socket {}", socket_path.display()))?;
    }

    let listener = UnixListener::bind(socket_path).with_context(|| {
        format!(
            "failed to bind publisher socket at {}",
            socket_path.display()
        )
    })?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("failed to chmod {}", socket_path.display()))?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept publisher connection")?;
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, config).await {
                tracing::error!(error = %err, "publisher request failed");
            }
        });
    }
}

pub async fn submit_publish_request(
    socket_path: &Path,
    request: &PublishPrRequest,
) -> Result<PublishPrResponse> {
    let payload = serde_json::to_vec(request).context("failed to serialize publish request")?;
    let response = submit_publisher_payload(socket_path, &payload).await?;
    match serde_json::from_slice::<PublisherResponse>(&response) {
        Ok(PublisherResponse::PublishPr(response)) => Ok(response),
        Ok(PublisherResponse::DirectPush(_)) => {
            bail!("publisher returned unexpected response type")
        }
        Err(_) => serde_json::from_slice(&response).context("failed to decode publish response"),
    }
}

pub async fn submit_direct_push_request(
    socket_path: &Path,
    request: &DirectPushRequest,
) -> Result<DirectPushResponse> {
    let payload = encode_direct_push_wire_request(request)?;
    let response = submit_publisher_payload(socket_path, &payload).await?;
    match serde_json::from_slice::<PublisherResponse>(&response) {
        Ok(PublisherResponse::DirectPush(response)) => Ok(response),
        Ok(PublisherResponse::PublishPr(_)) => bail!("publisher returned unexpected response type"),
        Err(_) => {
            serde_json::from_slice(&response).context("failed to decode direct push response")
        }
    }
}

pub(super) fn encode_direct_push_wire_request(request: &DirectPushRequest) -> Result<Vec<u8>> {
    let mut value =
        serde_json::to_value(request).context("failed to encode direct push request")?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("direct push request did not encode as an object"))?;
    object.insert(
        "kind".to_string(),
        serde_json::Value::String("direct_push".to_string()),
    );
    serde_json::to_vec(&value).context("failed to serialize direct push request")
}

async fn submit_publisher_payload(socket_path: &Path, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_SOCKET_REQUEST_BYTES {
        bail!("publisher request exceeds local socket limit");
    }

    let mut stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to publisher socket {}",
            socket_path.display()
        )
    })?;
    stream
        .write_all(payload)
        .await
        .context("failed to write publish request")?;
    stream
        .shutdown()
        .await
        .context("failed to close publisher request stream")?;

    let mut response_buf = Vec::new();
    stream
        .read_to_end(&mut response_buf)
        .await
        .context("failed to read publisher response")?;
    if response_buf.is_empty() {
        bail!("publisher returned an empty response");
    }

    if let Ok(error) = serde_json::from_slice::<PublishPrError>(&response_buf) {
        bail!(error.error);
    }

    Ok(response_buf)
}
async fn handle_connection(mut stream: UnixStream, config: Config) -> Result<()> {
    let mut request_bytes = Vec::new();
    stream
        .read_to_end(&mut request_bytes)
        .await
        .context("failed to read publisher request")?;

    let response = match decode_request(&request_bytes) {
        Ok(PublisherRequest::PublishPr(request)) => {
            match validate_publish_request(&config, &request).map(|validated| (request, validated))
            {
                Ok((request, (target, bundle_bytes))) => {
                    match handle_publish_request(&config, request, &target, &bundle_bytes).await {
                        Ok(response) => serde_json::to_vec(&PublisherResponse::PublishPr(response))
                            .context("failed to encode publish response")?,
                        Err(err) => encode_publisher_error("publish-pr", &err)?,
                    }
                }
                Err(err) => encode_publisher_error("publish-pr validation", &err)?,
            }
        }
        Ok(PublisherRequest::DirectPush(request)) => {
            match handle_direct_push_request(&config, request).await {
                Ok(response) => serde_json::to_vec(&PublisherResponse::DirectPush(response))
                    .context("failed to encode direct push response")?,
                Err(err) => encode_publisher_error("direct push", &err)?,
            }
        }
        Err(err) => encode_publisher_error("publisher request decode", &err)?,
    };

    stream
        .write_all(&response)
        .await
        .context("failed to write publisher response")?;
    stream
        .shutdown()
        .await
        .context("failed to close publisher response stream")?;
    Ok(())
}

fn encode_publisher_error(operation: &str, err: &anyhow::Error) -> Result<Vec<u8>> {
    let error = error_chain_string(err);
    tracing::error!(operation, error = %error, "publisher operation failed");
    serde_json::to_vec(&PublishPrError { error })
        .context("failed to encode publisher error response")
}

fn error_chain_string(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

pub(super) fn decode_request(request_bytes: &[u8]) -> Result<PublisherRequest> {
    if request_bytes.is_empty() {
        bail!("publisher request body was empty");
    }
    if request_bytes.len() > MAX_SOCKET_REQUEST_BYTES {
        bail!("publisher request exceeds socket size limit");
    }

    let value: serde_json::Value =
        serde_json::from_slice(request_bytes).context("publisher request was not valid JSON")?;
    match value.get("kind").and_then(|kind| kind.as_str()) {
        Some("direct_push") => {
            let request: DirectPushRequest =
                serde_json::from_value(value).context("failed to decode direct push request")?;
            return Ok(PublisherRequest::DirectPush(request));
        }
        Some("publish_pr") => {
            let request: PublishPrRequest =
                serde_json::from_value(value).context("failed to decode publish request")?;
            return Ok(PublisherRequest::PublishPr(request));
        }
        Some(kind) => bail!("unsupported publisher request kind: {kind}"),
        None => {}
    }

    let legacy: PublishPrRequest =
        serde_json::from_value(value).context("failed to decode publish request")?;
    Ok(PublisherRequest::PublishPr(legacy))
}

async fn handle_direct_push_request(
    config: &Config,
    request: DirectPushRequest,
) -> Result<DirectPushResponse> {
    let target = validate_direct_push_request(config, &request)?;
    let token = mint_publisher_installation_token(
        config
            .publisher_app_id
            .ok_or_else(|| anyhow!("publisher_app_id is not configured"))?,
        Path::new(&config.publisher_private_key_path),
        target.installation_id,
    )
    .await?;

    let tempdir = tempdir().context("failed to create publisher tempdir")?;
    let askpass_path = write_askpass_script(tempdir.path())?;
    let repo_dir = clone_repo_with_token(tempdir.path(), &token, &askpass_path, &target.repo)?;

    if request.src.is_empty() {
        git_with_token(
            &repo_dir,
            &token,
            &askpass_path,
            &[
                "push",
                &github_repo_https_url(&target.repo),
                &format!(":{}", request.dst),
            ],
        )?;
    } else {
        let push_src = if let Some(bundle_base64) = request.bundle_base64.as_deref() {
            let bundle_bytes = BASE64
                .decode(bundle_base64.as_bytes())
                .context("direct push bundle was not valid base64")?;
            if bundle_bytes.is_empty() {
                bail!("direct push bundle cannot be empty");
            }
            if bundle_bytes.len() > config.publisher_max_bundle_bytes {
                bail!(
                    "direct push bundle exceeds limit ({} bytes > {} bytes)",
                    bundle_bytes.len(),
                    config.publisher_max_bundle_bytes
                );
            }
            let bundle_path = tempdir.path().join("direct-push.bundle");
            fs::write(&bundle_path, bundle_bytes)
                .with_context(|| format!("failed to write {}", bundle_path.display()))?;
            git_plain(
                &repo_dir,
                &[
                    "fetch",
                    bundle_path.to_str().unwrap(),
                    &format!("{}:{DIRECT_PUSH_IMPORTED_REF}", request.src),
                ],
            )?;
            DIRECT_PUSH_IMPORTED_REF.to_string()
        } else {
            let src_oid = request.src_oid.as_deref().ok_or_else(|| {
                anyhow!(
                    "direct push bundle is empty and no source object id was provided; this usually means the pushed ref points to an object already present on the remote"
                )
            })?;
            validate_git_object_id(src_oid)?;
            git_plain(&repo_dir, &["cat-file", "-e", &format!("{src_oid}^{{object}}")])
                .with_context(|| {
                    format!(
                        "direct push source object {src_oid} is not present in the publisher clone; retry after pushing the containing branch or include the object in the push bundle"
                    )
                })?;
            src_oid.to_string()
        };
        let refspec = if request.force {
            format!("+{push_src}:{}", request.dst)
        } else {
            format!("{push_src}:{}", request.dst)
        };
        git_with_token(
            &repo_dir,
            &token,
            &askpass_path,
            &["push", &github_repo_https_url(&target.repo), &refspec],
        )?;
    }

    Ok(DirectPushResponse {
        repo: target.repo,
        dst: request.dst,
    })
}

async fn handle_publish_request(
    config: &Config,
    request: PublishPrRequest,
    target: &PublishTarget,
    bundle_bytes: &[u8],
) -> Result<PublishPrResponse> {
    let token = mint_publisher_installation_token(
        config
            .publisher_app_id
            .ok_or_else(|| anyhow!("publisher_app_id is not configured"))?,
        Path::new(&config.publisher_private_key_path),
        target.installation_id,
    )
    .await?;

    let tempdir = tempdir().context("failed to create publisher tempdir")?;
    let askpass_path = write_askpass_script(tempdir.path())?;
    let bundle_path = tempdir.path().join("request.bundle");
    fs::write(&bundle_path, bundle_bytes)
        .with_context(|| format!("failed to write {}", bundle_path.display()))?;

    let repo_dir = tempdir.path().join("repo");
    git_with_token(
        tempdir.path(),
        &token,
        &askpass_path,
        &[
            "clone",
            "--quiet",
            &github_repo_https_url(&target.repo),
            repo_dir.to_str().unwrap(),
        ],
    )?;

    git_plain(
        &repo_dir,
        &[
            "fetch",
            bundle_path.to_str().unwrap(),
            &format!("HEAD:{IMPORTED_REF}"),
        ],
    )?;

    let branch = build_publish_branch_name(&config.publisher_branch_prefix);
    git_plain(&repo_dir, &["checkout", "-B", &branch, IMPORTED_REF])?;
    git_with_token(
        &repo_dir,
        &token,
        &askpass_path,
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
    )?;

    let pr = create_pull_request(
        &token,
        &target.repo,
        request.base.as_deref().unwrap_or(&target.default_base),
        &branch,
        &request.title,
        &request.body,
        request.draft,
    )
    .await?;

    Ok(pr)
}
