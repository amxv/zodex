fn resolve_publisher_client_id(
    config: &Config,
    publisher_client_id: Option<&str>,
) -> Option<String> {
    publisher_client_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            env::var(GITHUB_PUSH_GRANT_CLIENT_ID_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| config.publisher_client_id.clone())
}

fn push_grant_cache_path(repo: &str) -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME must be set to use GitHub App device flow")?;
    let root = Path::new(&home).join(GITHUB_PUSH_GRANT_DEVICE_CACHE_DIR);
    Ok(root.join(push_grant_file_name(repo)))
}

fn save_cached_device_flow_grant(repo: &str, grant: &CachedDeviceFlowGrant) -> Result<()> {
    let path = push_grant_cache_path(repo)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw =
        serde_json::to_vec_pretty(grant).context("failed to encode cached device-flow grant")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn load_cached_device_flow_grant(
    repo: &str,
    client_id: &str,
) -> Result<Option<CachedDeviceFlowGrant>> {
    let path = push_grant_cache_path(repo)?;
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let grant: CachedDeviceFlowGrant = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if grant.client_id != client_id {
        return Ok(None);
    }
    Ok(Some(grant))
}

fn remove_cached_device_flow_grant(repo: &str) -> Result<bool> {
    let path = push_grant_cache_path(repo)?;
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    Ok(true)
}

fn best_effort_open_browser(url: &str) -> bool {
    for (program, args) in browser_open_attempts(url) {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            return true;
        }
    }
    false
}

fn browser_open_attempts(url: &str) -> Vec<(&'static str, Vec<&str>)> {
    if cfg!(target_os = "macos") {
        return vec![("open", vec![url])];
    }
    if cfg!(target_os = "windows") {
        return vec![("cmd", vec!["/C", "start", "", url])];
    }

    let mut attempts = Vec::new();
    if env::var_os("WSL_DISTRO_NAME").is_some() {
        attempts.push(("wslview", vec![url]));
        attempts.push((
            "powershell.exe",
            vec!["-NoProfile", "-Command", "Start-Process", url],
        ));
    }
    attempts.push(("xdg-open", vec![url]));
    attempts
}

fn best_effort_copy_to_clipboard(text: &str) -> bool {
    for (program, args) in clipboard_copy_attempts() {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };

        let write_result = child
            .stdin
            .as_mut()
            .map(|stdin| stdin.write_all(text.as_bytes()))
            .transpose();
        let wait_result = child.wait();
        if write_result.is_ok() && matches!(wait_result, Ok(status) if status.success()) {
            return true;
        }
    }
    false
}

fn clipboard_copy_attempts() -> Vec<(&'static str, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", vec![])];
    }
    if cfg!(target_os = "windows") {
        return vec![("clip.exe", vec![])];
    }

    let mut attempts = Vec::new();
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        attempts.push(("wl-copy", vec![]));
    }
    if env::var_os("DISPLAY").is_some() {
        attempts.push(("xclip", vec!["-selection", "clipboard"]));
        attempts.push(("xsel", vec!["--clipboard", "--input"]));
    }
    if env::var_os("WSL_DISTRO_NAME").is_some() {
        attempts.push(("clip.exe", vec![]));
    }
    attempts
}

#[derive(Debug, Deserialize)]
struct GitHubRepoResponse {
    id: u64,
}

#[derive(Debug)]
struct GitHubUserAccessGrant {
    access_token: String,
    expires_in_seconds: Option<u64>,
    refresh_token: Option<String>,
}

async fn github_repo_id(repo: &str, bearer_token: Option<&str>) -> Result<Option<u64>> {
    let client = reqwest::Client::new();
    let mut request = client
        .get(format!("{GITHUB_API_BASE}/repos/{repo}"))
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT);
    if let Some(token) = bearer_token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = request
        .send()
        .await
        .context("failed to resolve GitHub repository metadata")?;

    if response.status().as_u16() == 404 || response.status().as_u16() == 403 {
        return Ok(None);
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub repository lookup failed ({status}): {body}");
    }

    let payload: GitHubRepoResponse = response
        .json()
        .await
        .context("failed to decode GitHub repository metadata")?;
    Ok(Some(payload.id))
}

async fn try_resolve_repo_id_for_device_flow(config: &Config, repo: &str) -> Result<Option<u64>> {
    if let Some(repo_id) = github_repo_id(repo, None).await? {
        return Ok(Some(repo_id));
    }

    if let (Some(app_id), Some(installation_id)) =
        (config.reader_app_id, config.reader_installation_id)
        && Path::new(&config.reader_private_key_path).exists()
    {
        let token = mint_reader_installation_token(
            app_id,
            Path::new(&config.reader_private_key_path),
            installation_id,
        )
        .await?;
        if let Some(repo_id) = github_repo_id(repo, Some(&token)).await? {
            return Ok(Some(repo_id));
        }
    }

    Ok(None)
}

async fn request_device_flow_code(client_id: &str) -> Result<GitHubDeviceCodeResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(GITHUB_OAUTH_DEVICE_CODE_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&[("client_id", client_id)])
        .send()
        .await
        .context("failed to request GitHub device code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub device code request failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub device code response")
}

async fn poll_device_flow_access_token(
    client_id: &str,
    device_code: &str,
    repository_id: Option<u64>,
) -> Result<GitHubOAuthTokenResponse> {
    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.to_string()),
        ("device_code", device_code.to_string()),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        ),
    ];
    if let Some(repository_id) = repository_id {
        params.push(("repository_id", repository_id.to_string()));
    }

    let response = client
        .post(GITHUB_OAUTH_ACCESS_TOKEN_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&params)
        .send()
        .await
        .context("failed to poll GitHub device flow token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub device flow token request failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub device flow token response")
}

async fn refresh_user_access_token(
    client_id: &str,
    refresh_token: &str,
) -> Result<GitHubOAuthTokenResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(GITHUB_OAUTH_ACCESS_TOKEN_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("failed to refresh GitHub user access token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub user access token refresh failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub user access token refresh response")
}

fn oauth_token_response_error(response: &GitHubOAuthTokenResponse) -> Option<&str> {
    response.error.as_deref()
}

fn grant_expiration_from_expires_in(
    expires_in: Option<u64>,
) -> Result<(Option<String>, Option<u64>)> {
    match expires_in {
        Some(expires_in) => {
            let (formatted, epoch_seconds) = expires_at_from_now(expires_in)?;
            Ok((Some(formatted), Some(epoch_seconds)))
        }
        None => Ok((None, None)),
    }
}

async fn mint_user_access_token_via_device_flow(
    client_id: &str,
    repo: &str,
    repository_id: Option<u64>,
) -> Result<GitHubUserAccessGrant> {
    let code = request_device_flow_code(client_id).await?;
    let opened_browser = best_effort_open_browser(&code.verification_uri);
    let copied_code = best_effort_copy_to_clipboard(&code.user_code);
    println!("github-device-flow: pending");
    println!("repo: {repo}");
    if let Some(repository_id) = repository_id {
        println!("repository-id: {repository_id}");
    } else {
        println!("repository-id: unresolved");
        println!(
            "note: GitHub-side token narrowing could not be confirmed; Sprite delivery remains repo-scoped"
        );
    }
    println!("verification-uri: {}", code.verification_uri);
    println!("user-code: {}", code.user_code);
    println!("expires-in-seconds: {}", code.expires_in);
    println!(
        "verification-uri-opened: {}",
        if opened_browser { "yes" } else { "no" }
    );
    println!(
        "user-code-copied: {}",
        if copied_code { "yes" } else { "no" }
    );
    if !opened_browser {
        println!("note: open the verification URI manually if a browser did not launch");
    }
    if !copied_code {
        println!("note: copy the user code manually if clipboard integration is unavailable");
    }

    let mut interval_seconds = code.interval.unwrap_or(5).max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval_seconds)).await;
        let response =
            poll_device_flow_access_token(client_id, &code.device_code, repository_id).await?;
        match oauth_token_response_error(&response) {
            None => {
                let access_token = response.access_token.ok_or_else(|| {
                    anyhow!("GitHub device flow completed without an access token")
                })?;
                return Ok(GitHubUserAccessGrant {
                    access_token,
                    expires_in_seconds: response.expires_in,
                    refresh_token: response.refresh_token,
                });
            }
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval_seconds = response
                    .interval
                    .unwrap_or(interval_seconds + 5)
                    .max(interval_seconds + 1);
            }
            Some("expired_token") | Some("token_expired") => {
                bail!("GitHub device flow code expired before authorization completed");
            }
            Some("access_denied") => bail!("GitHub device flow authorization was cancelled"),
            Some("device_flow_disabled") => {
                bail!("GitHub App device flow is disabled; enable device flow in the app settings")
            }
            Some(other) => {
                let details = response
                    .error_description
                    .as_deref()
                    .unwrap_or("no description");
                bail!("GitHub device flow failed with {other}: {details}");
            }
        }
    }
}

async fn mint_device_flow_push_grant(
    config: &Config,
    repo: &str,
    client_id: &str,
    persist_refresh_token: bool,
    active_ttl: Option<Duration>,
) -> Result<PushGrantRecord> {
    if let Some(cached) = load_cached_device_flow_grant(repo, client_id)? {
        let refreshed = refresh_user_access_token(client_id, &cached.refresh_token).await;
        match refreshed {
            Ok(response) if response.error.is_none() => {
                let access_token = response
                    .access_token
                    .ok_or_else(|| anyhow!("GitHub refresh completed without an access token"))?;
                if persist_refresh_token && let Some(refresh_token) = response.refresh_token.clone()
                {
                    save_cached_device_flow_grant(
                        repo,
                        &CachedDeviceFlowGrant {
                            client_id: client_id.to_string(),
                            repo: repo.to_string(),
                            refresh_token,
                        },
                    )?;
                }
                let (expires_at, expires_at_epoch_seconds) = match active_ttl {
                    Some(active_ttl) => {
                        let (formatted, epoch_seconds) = expires_at_from_now(active_ttl.as_secs())?;
                        (Some(formatted), Some(epoch_seconds))
                    }
                    None => grant_expiration_from_expires_in(response.expires_in)?,
                };
                return Ok(PushGrantRecord {
                    repo: repo.to_string(),
                    token: access_token,
                    expires_at,
                    expires_at_epoch_seconds,
                    token_source: Some("github-app-user-token".to_string()),
                });
            }
            Ok(response)
                if matches!(
                    oauth_token_response_error(&response),
                    Some("bad_refresh_token")
                ) =>
            {
                remove_cached_device_flow_grant(repo)?;
            }
            Ok(response) => {
                let error = response
                    .error
                    .unwrap_or_else(|| "unknown_error".to_string());
                let details = response
                    .error_description
                    .unwrap_or_else(|| "no description".to_string());
                bail!("GitHub user access token refresh failed with {error}: {details}");
            }
            Err(err) => {
                let message = err.to_string();
                if message.contains("incorrect_client_credentials")
                    || message.contains("bad_refresh_token")
                {
                    remove_cached_device_flow_grant(repo)?;
                } else {
                    return Err(err);
                }
            }
        }
    }

    let repository_id = try_resolve_repo_id_for_device_flow(config, repo).await?;
    let grant = mint_user_access_token_via_device_flow(client_id, repo, repository_id).await?;
    if persist_refresh_token && let Some(refresh_token) = grant.refresh_token.clone() {
        save_cached_device_flow_grant(
            repo,
            &CachedDeviceFlowGrant {
                client_id: client_id.to_string(),
                repo: repo.to_string(),
                refresh_token,
            },
        )?;
    }
    let (expires_at, expires_at_epoch_seconds) = match active_ttl {
        Some(active_ttl) => {
            let (formatted, epoch_seconds) = expires_at_from_now(active_ttl.as_secs())?;
            (Some(formatted), Some(epoch_seconds))
        }
        None => grant_expiration_from_expires_in(grant.expires_in_seconds)?,
    };

    Ok(PushGrantRecord {
        repo: repo.to_string(),
        token: grant.access_token,
        expires_at,
        expires_at_epoch_seconds,
        token_source: Some("github-app-user-token".to_string()),
    })
}

async fn request_push_access(
    config: &Config,
    repo: &str,
    publisher_client_id: Option<&str>,
    active_ttl: Option<Duration>,
    cache_refresh_token: bool,
) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    let client_id = resolve_publisher_client_id(config, publisher_client_id).ok_or_else(|| {
        anyhow!(
            "publisher client id is required for device-flow push grants; set `publisher_client_id`, pass `--publisher-client-id`, or export {GITHUB_PUSH_GRANT_CLIENT_ID_ENV}"
        )
    })?;
    let grant =
        mint_device_flow_push_grant(config, &repo, &client_id, cache_refresh_token, active_ttl)
            .await?;
    write_local_push_grant(&repo, &grant)?;

    println!("push-grant: active");
    println!("repo: {repo}");
    println!("grant-location: local");
    println!(
        "ttl: {}",
        if active_ttl.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "refresh-token-cache: {}",
        if cache_refresh_token {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "token-source: {}",
        grant
            .token_source
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    );
    if let Some(expires_at) = grant.expires_at.as_deref() {
        println!("expires-at: {expires_at}");
    }
    Ok(())
}

async fn grant_push_access(
    config: &Config,
    sprite: &str,
    org: Option<&str>,
    repo: &str,
    publisher_client_id: Option<&str>,
) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    let client_id = resolve_publisher_client_id(config, publisher_client_id).ok_or_else(|| {
        anyhow!(
            "publisher client id is required for device-flow push grants; set `publisher_client_id`, pass `--publisher-client-id`, or export {GITHUB_PUSH_GRANT_CLIENT_ID_ENV}"
        )
    })?;
    let grant = mint_device_flow_push_grant(config, &repo, &client_id, true, None).await?;
    let raw = serde_json::to_string(&grant).context("failed to serialize push grant")?;
    let mut grant_file = NamedTempFile::new().context("failed to create grant temp file")?;
    use std::io::Write as _;
    grant_file
        .write_all(raw.as_bytes())
        .context("failed to write grant temp file")?;

    let exec_args = vec![
        "bash".to_string(),
        "-lc".to_string(),
        format!(
            "sudo install -d -m 0750 -o zodex-agent -g zodex {dir} && sudo install -m 0640 -o zodex-agent -g zodex {tmp} {dest} && rm -f {tmp}",
            dir = PUSH_GRANTS_DIR,
            tmp = PUSH_GRANT_REMOTE_TMP_PATH,
            dest = push_grant_path(&repo).display()
        ),
    ];
    run_sprite_exec(
        sprite,
        org,
        &exec_args,
        &[(grant_file.path(), PUSH_GRANT_REMOTE_TMP_PATH)],
    )?;

    println!("push-grant: active");
    println!("repo: {repo}");
    println!(
        "token-source: {}",
        grant
            .token_source
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    );
    if let Some(expires_at) = grant.expires_at.as_deref() {
        println!("expires-at: {expires_at}");
    }
    Ok(())
}

fn revoke_push_access(
    sprite: Option<&str>,
    org: Option<&str>,
    repo: &str,
    forget_local_auth: bool,
) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    match sprite {
        Some(sprite) => {
            let exec_args = vec![
                "bash".to_string(),
                "-lc".to_string(),
                format!("sudo rm -f {}", push_grant_path(&repo).display()),
            ];
            run_sprite_exec(sprite, org, &exec_args, &[])?;
            println!("grant-location: sprite");
        }
        None if sprite_runtime_detected() => {
            let path = push_grant_path(&repo);
            let removed = if path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
                true
            } else {
                false
            };
            println!("grant-location: local");
            println!(
                "push-grant-file: {}",
                if removed { "removed" } else { "not-found" }
            );
        }
        None => {
            let resolved = resolve_remote_sprite(None, org)?;
            let exec_args = vec![
                "bash".to_string(),
                "-lc".to_string(),
                format!("sudo rm -f {}", push_grant_path(&repo).display()),
            ];
            run_sprite_exec(&resolved.name, resolved.org.as_deref(), &exec_args, &[])?;
            println!("grant-location: sprite");
            println!("sprite: {}", resolved.name);
        }
    }
    println!("push-grant: revoked");
    println!("repo: {repo}");
    if forget_local_auth {
        let removed_local_state = remove_cached_device_flow_grant(&repo)?;
        println!(
            "local-device-flow-state: {}",
            if removed_local_state {
                "removed"
            } else {
                "not-found"
            }
        );
    } else {
        println!("local-device-flow-state: retained");
        println!("note: pass --forget-local-auth to remove the cached local refresh token too");
    }
    Ok(())
}

fn list_push_grants(sprite: Option<&str>, org: Option<&str>) -> Result<()> {
    let raw = match sprite {
        Some(sprite) => {
            let exec_args = vec![
                "bash".to_string(),
                "-lc".to_string(),
                format!(
                    "if [[ -d {dir} ]]; then shopt -s nullglob; for file in {dir}/*.json; do cat \"$file\"; echo; done; fi",
                    dir = PUSH_GRANTS_DIR
                ),
            ];
            println!("grant-location: sprite");
            run_sprite_exec(sprite, org, &exec_args, &[])?
        }
        None if !sprite_runtime_detected() => {
            let resolved = resolve_remote_sprite(None, org)?;
            let exec_args = vec![
                "bash".to_string(),
                "-lc".to_string(),
                format!(
                    "if [[ -d {dir} ]]; then shopt -s nullglob; for file in {dir}/*.json; do cat \"$file\"; echo; done; fi",
                    dir = PUSH_GRANTS_DIR
                ),
            ];
            println!("grant-location: sprite");
            println!("sprite: {}", resolved.name);
            run_sprite_exec(&resolved.name, resolved.org.as_deref(), &exec_args, &[])?
        }
        None if sprite_runtime_detected() => {
            println!("grant-location: local");
            let grants_dir = Path::new(PUSH_GRANTS_DIR);
            if !grants_dir.is_dir() {
                String::new()
            } else {
                let mut blobs = Vec::new();
                for entry in fs::read_dir(grants_dir)
                    .with_context(|| format!("failed to read {}", grants_dir.display()))?
                {
                    let entry = entry
                        .with_context(|| format!("failed to read {}", grants_dir.display()))?;
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                        continue;
                    }
                    blobs.push(
                        fs::read_to_string(&path)
                            .with_context(|| format!("failed to read {}", path.display()))?,
                    );
                }
                blobs.join("\n")
            }
        }
        None => {
            bail!(
                "pass `--sprite <name>` to inspect a remote Sprite grant set, or run this command on the Sprite to inspect local grants"
            );
        }
    };
    let mut grants = Vec::new();
    for grant in parse_push_grants(&raw)? {
        if push_grant_expired(&grant, current_epoch_seconds()?) {
            continue;
        }
        grants.push(grant);
    }

    if grants.is_empty() {
        println!("push-grants: none");
        return Ok(());
    }

    for grant in grants {
        println!("repo: {}", grant.repo);
        if let Some(source) = grant.token_source.as_deref() {
            println!("token-source: {source}");
        }
        println!(
            "expires-at: {}",
            grant.expires_at.unwrap_or_else(|| "unknown".to_string())
        );
        println!();
    }
    Ok(())
}
