#[derive(Debug, Clone, PartialEq, Eq)]
struct SpriteUrlInfo {
    url: Option<String>,
    auth: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyOriginResolution {
    origin: String,
    sprite_url_auth: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyDeployCommandSpec {
    program: String,
    base_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyOriginCheck {
    origin: String,
    sprite_url_auth: Option<String>,
    health_status: u16,
    mcp_status: u16,
    mcp_slash_status: u16,
}

fn validate_sprite_url_auth(url_auth: &str) -> Result<()> {
    if matches!(url_auth, "sprite" | "public") {
        Ok(())
    } else {
        bail!("url auth must be `sprite` or `public`");
    }
}

fn build_sprite_scope_args(sprite: &str, org: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(org) = org {
        args.push("-o".to_string());
        args.push(org.to_string());
    }
    args.push("-s".to_string());
    args.push(sprite.to_string());
    args
}

fn run_sprite_exec(
    sprite: &str,
    org: Option<&str>,
    exec_args: &[String],
    uploads: &[(&Path, &str)],
) -> Result<String> {
    let mut args = build_sprite_scope_args(sprite, org);
    args.push("exec".to_string());
    for (local, remote) in uploads {
        args.push("--file".to_string());
        args.push(format!("{}:{remote}", local.display()));
    }
    args.push("--".to_string());
    args.extend(exec_args.iter().cloned());
    run_command_capture("sprite", &args)
}

fn sprite_url_info(sprite: &str, org: Option<&str>) -> Result<SpriteUrlInfo> {
    let mut args = build_sprite_scope_args(sprite, org);
    args.push("url".to_string());
    let raw = run_command_capture("sprite", &args)?;
    let mut info = SpriteUrlInfo {
        url: None,
        auth: None,
    };
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("URL:") {
            info.url = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Auth:") {
            info.auth = Some(value.trim().to_string());
        }
    }
    Ok(info)
}

fn set_sprite_url_auth(sprite: &str, org: Option<&str>, url_auth: &str) -> Result<()> {
    validate_sprite_url_auth(url_auth)?;
    let mut args = build_sprite_scope_args(sprite, org);
    args.extend([
        "url".to_string(),
        "update".to_string(),
        "--auth".to_string(),
        url_auth.to_string(),
    ]);
    run_command_capture("sprite", &args)?;
    Ok(())
}

fn proxy_component_dir() -> PathBuf {
    manifest_dir().join(PROXY_COMPONENT_DIR)
}

fn proxy_component_readme_path() -> PathBuf {
    manifest_dir().join(PROXY_COMPONENT_README)
}

fn proxy_worker_entrypoint_path() -> PathBuf {
    manifest_dir().join(PROXY_WORKER_ENTRYPOINT)
}

fn proxy_wrangler_template_path() -> PathBuf {
    manifest_dir().join(PROXY_WRANGLER_TEMPLATE_PATH)
}

fn normalize_proxy_origin(origin: &str) -> Result<String> {
    let parsed = Url::parse(origin)
        .with_context(|| format!("failed to parse proxy origin URL `{origin}`"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        bail!("proxy origin must use http or https");
    }
    if parsed.host_str().is_none() {
        bail!("proxy origin must include a host");
    }
    if parsed.path() != "/" && !parsed.path().is_empty() {
        bail!("proxy origin must not include a path; pass the Sprite base URL only");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("proxy origin must not include a query string or fragment");
    }

    let mut normalized = parsed;
    normalized.set_path("");
    Ok(normalized.to_string().trim_end_matches('/').to_string())
}

fn resolve_proxy_origin(
    sprite: Option<&str>,
    org: Option<&str>,
    origin: Option<&str>,
) -> Result<ProxyOriginResolution> {
    if let Some(origin) = origin {
        return Ok(ProxyOriginResolution {
            origin: normalize_proxy_origin(origin)?,
            sprite_url_auth: None,
        });
    }

    let sprite = sprite.ok_or_else(|| {
        anyhow!(
            "pass either `--origin <sprite-url>` or `--sprite <name>` to resolve the proxy target"
        )
    })?;
    let info = sprite_url_info(sprite, org)?;
    let url = info
        .url
        .ok_or_else(|| anyhow!("sprite URL is not available for {sprite}"))?;
    Ok(ProxyOriginResolution {
        origin: normalize_proxy_origin(&url)?,
        sprite_url_auth: info.auth,
    })
}

fn inspect_proxy_component(
    sprite: Option<&str>,
    org: Option<&str>,
    origin: Option<&str>,
) -> Result<()> {
    let resolved_origin = resolve_proxy_origin(sprite, org, origin).ok();
    println!("component: zodex proxy");
    println!("directory: {}", proxy_component_dir().display());
    println!("entrypoint: {}", proxy_worker_entrypoint_path().display());
    println!(
        "wrangler-config-template: {}",
        proxy_wrangler_template_path().display()
    );
    println!("readme: {}", proxy_component_readme_path().display());
    println!("routes: /health, /mcp, /mcp/");
    println!(
        "responsibilities: path-normalization, cold-wake-warmup, retry, streaming-preservation"
    );
    match resolved_origin {
        Some(resolution) => {
            println!("resolved-sprite-origin: {}", resolution.origin);
            if let Some(auth) = resolution.sprite_url_auth {
                println!("sprite-url-auth: {auth}");
            }
        }
        None => {
            println!("resolved-sprite-origin: <pass --sprite or --origin to resolve>");
        }
    }

    match resolve_proxy_deploy_command() {
        Ok(command) => {
            println!(
                "deploy-runner: {} {}",
                command.program,
                command.base_args.join(" ")
            );
        }
        Err(err) => {
            println!("deploy-runner: unavailable");
            println!("hint: {err}");
        }
    }
    println!("deploy-command: cd proxy/cloudflare-worker && npx wrangler deploy");
    println!("deploy-config: set vars.SPRITE_ORIGIN in wrangler.jsonc first");
    println!("verify-command: zodex proxy verify-origin --sprite <sprite>");
    Ok(())
}

fn deploy_proxy_component(
    sprite: Option<&str>,
    org: Option<&str>,
    origin: Option<&str>,
    skip_verify_origin: bool,
) -> Result<()> {
    let resolution = resolve_proxy_origin(sprite, org, origin)?;
    ensure_proxy_origin_is_publicly_routable(&resolution)?;

    if !skip_verify_origin {
        let verification = verify_proxy_origin(&resolution)?;
        print_proxy_origin_check(&verification);
    }

    let template = fs::read_to_string(proxy_wrangler_template_path()).with_context(|| {
        format!(
            "failed to read {}",
            proxy_wrangler_template_path().display()
        )
    })?;
    let rendered_config = render_proxy_wrangler_config(&template, &resolution.origin)?;
    let mut temp_config = NamedTempFile::new_in(proxy_component_dir())
        .context("failed to create temporary Wrangler config")?;
    temp_config
        .write_all(rendered_config.as_bytes())
        .context("failed to write temporary Wrangler config")?;

    let deploy = resolve_proxy_deploy_command()?;
    let mut args = deploy.base_args.clone();
    args.extend([
        "deploy".to_string(),
        "--config".to_string(),
        temp_config.path().display().to_string(),
    ]);

    let output = run_command_capture_with(&deploy.program, &args, Some(&proxy_component_dir()))?;
    print!("{output}");
    println!("proxy-origin: {}", resolution.origin);
    println!("proxy-deploy: complete");
    Ok(())
}

fn render_proxy_wrangler_config(template: &str, origin: &str) -> Result<String> {
    if !template.contains(PROXY_SPRITE_ORIGIN_PLACEHOLDER) {
        bail!(
            "proxy wrangler template is missing placeholder {}",
            PROXY_SPRITE_ORIGIN_PLACEHOLDER
        );
    }
    Ok(template.replace(PROXY_SPRITE_ORIGIN_PLACEHOLDER, origin))
}

fn verify_proxy_origin_command(
    sprite: Option<&str>,
    org: Option<&str>,
    origin: Option<&str>,
) -> Result<()> {
    let resolution = resolve_proxy_origin(sprite, org, origin)?;
    let verification = verify_proxy_origin(&resolution)?;
    print_proxy_origin_check(&verification);
    Ok(())
}

fn ensure_proxy_origin_is_publicly_routable(resolution: &ProxyOriginResolution) -> Result<()> {
    if let Some(auth) = resolution.sprite_url_auth.as_deref()
        && auth != "public"
    {
        bail!(
            "sprite URL auth is `{auth}` for {}. Proxy deploy expects a publicly reachable Sprite URL. Set the Sprite URL auth to `public` before deploying the Worker.",
            resolution.origin
        );
    }
    Ok(())
}

fn verify_proxy_origin(resolution: &ProxyOriginResolution) -> Result<ProxyOriginCheck> {
    let base = resolution.origin.trim_end_matches('/');
    let health_status = probe_http_status(&format!("{base}/health"))?;
    let mcp_status = probe_http_status(&format!("{base}/mcp"))?;
    let mcp_slash_status = probe_http_status(&format!("{base}/mcp/"))?;

    if health_status != 200 {
        bail!("raw Sprite origin health probe returned HTTP {health_status} for {base}/health");
    }
    if !proxy_mcp_status_looks_healthy(mcp_status) {
        bail!("raw Sprite origin `/mcp` probe returned HTTP {mcp_status}; expected 200 or 401");
    }
    if !proxy_mcp_status_looks_healthy(mcp_slash_status) {
        bail!(
            "raw Sprite origin `/mcp/` probe returned HTTP {mcp_slash_status}; expected 200 or 401"
        );
    }

    Ok(ProxyOriginCheck {
        origin: resolution.origin.clone(),
        sprite_url_auth: resolution.sprite_url_auth.clone(),
        health_status,
        mcp_status,
        mcp_slash_status,
    })
}

fn print_proxy_origin_check(check: &ProxyOriginCheck) {
    println!("origin: {}", check.origin);
    if let Some(auth) = check.sprite_url_auth.as_deref() {
        println!("sprite-url-auth: {auth}");
    }
    println!("health-status: {}", check.health_status);
    println!("mcp-status: {}", check.mcp_status);
    println!("mcp-slash-status: {}", check.mcp_slash_status);
    if check.mcp_status != check.mcp_slash_status {
        println!(
            "route-note: `/mcp` and `/mcp/` differ at the raw Sprite edge; keep the proxy as the default front door"
        );
    } else {
        println!("route-note: raw Sprite `/mcp` and `/mcp/` matched on this probe");
    }
    println!("proxy-origin-check: ok");
}

fn proxy_mcp_status_looks_healthy(status: u16) -> bool {
    matches!(status, 200 | 401)
}

fn probe_http_status(url: &str) -> Result<u16> {
    let raw = run_command_capture(
        "curl",
        &[
            "-sS".to_string(),
            "-o".to_string(),
            "/dev/null".to_string(),
            "-w".to_string(),
            "%{http_code}".to_string(),
            "--max-time".to_string(),
            "20".to_string(),
            "--retry".to_string(),
            "2".to_string(),
            "--retry-delay".to_string(),
            "2".to_string(),
            "--retry-all-errors".to_string(),
            url.to_string(),
        ],
    )?;
    raw.trim()
        .parse::<u16>()
        .with_context(|| format!("failed to parse HTTP status from curl probe for {url}: {raw}"))
}

fn resolve_proxy_deploy_command() -> Result<ProxyDeployCommandSpec> {
    let local_wrangler = proxy_component_dir().join("node_modules/.bin/wrangler");
    if local_wrangler.is_file() {
        return Ok(ProxyDeployCommandSpec {
            program: local_wrangler.display().to_string(),
            base_args: Vec::new(),
        });
    }
    if command_exists("wrangler") {
        return Ok(ProxyDeployCommandSpec {
            program: "wrangler".to_string(),
            base_args: Vec::new(),
        });
    }
    if command_exists("bunx") {
        return Ok(ProxyDeployCommandSpec {
            program: "bunx".to_string(),
            base_args: vec!["wrangler".to_string()],
        });
    }
    if command_exists("npx") {
        return Ok(ProxyDeployCommandSpec {
            program: "npx".to_string(),
            base_args: vec!["--yes".to_string(), "wrangler".to_string()],
        });
    }

    bail!(
        "Wrangler was not found. Install it in `{}` or make `wrangler` available on PATH.",
        proxy_component_dir().display()
    )
}

fn derive_remote_target_repo(
    sprite: &str,
    org: Option<&str>,
    remote_config: &Path,
) -> Result<Option<String>> {
    let exec_args = vec![
        "sudo".to_string(),
        "awk".to_string(),
        "-F\"".to_string(),
        r#"/^\[\[publisher_targets\]\]/ { in_targets=1; next } in_targets && /^repo = "/ { print $2; exit }"#.to_string(),
        remote_config.display().to_string(),
    ];
    let raw = run_sprite_exec(sprite, org, &exec_args, &[])?;
    let repo = raw.trim();
    if repo.is_empty() {
        Ok(None)
    } else {
        Ok(Some(repo.to_string()))
    }
}

fn sync_sprite_services(
    sprite: &str,
    org: Option<&str>,
    config_path: &Path,
    force_recreate: bool,
    skip_stop_detached: bool,
) -> Result<()> {
    if !skip_stop_detached {
        let stop_args = vec![
            "sudo".to_string(),
            "bash".to_string(),
            "-lc".to_string(),
            format!(
                "pkill -f -- \"/usr/local/bin/zodexd --config {}\" || true; pkill -f -- \"/usr/local/bin/zodex-prd --config {}\" || true",
                config_path.display(),
                config_path.display()
            ),
        ];
        if let Err(err) = run_sprite_exec(sprite, org, &stop_args, &[]) {
            eprintln!("warning: failed to stop detached daemons before Sprite sync: {err}");
        }
    }

    if force_recreate {
        for service_name in [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL] {
            let status = run_sprite_api(
                sprite,
                org,
                &format!("/services/{service_name}"),
                &[
                    "-sS".to_string(),
                    "-o".to_string(),
                    "/dev/null".to_string(),
                    "-w".to_string(),
                    "%{http_code}\n".to_string(),
                    "-X".to_string(),
                    "DELETE".to_string(),
                ],
            )?;
            let trimmed = status.trim();
            if trimmed != "204" && trimmed != "404" {
                bail!("failed to delete Sprite service {service_name} (HTTP {trimmed})");
            }
        }
    }

    for (service_name, definition) in expected_sprite_service_definitions(config_path) {
        let payload = serde_json::to_string(&definition).context("failed to encode service")?;
        run_sprite_api(
            sprite,
            org,
            &format!("/services/{service_name}"),
            &[
                "-sS".to_string(),
                "-X".to_string(),
                "PUT".to_string(),
                "-H".to_string(),
                "Content-Type: application/json".to_string(),
                "-d".to_string(),
                payload,
            ],
        )?;
    }

    println!("sprite services synced for {sprite}");
    Ok(())
}

fn verify_sprite_service_logs(sprite: &str, org: Option<&str>) -> Result<()> {
    for service in [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL] {
        let path = sprite_service_logs_api_path(service, Some(20), None);
        run_sprite_api(sprite, org, &path, &["-sS".to_string()])?;
    }
    Ok(())
}

fn verify_local_sprite_health(sprite: &str, org: Option<&str>) -> Result<()> {
    let exec_args = vec![
        "sudo".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
        "curl -fsS http://127.0.0.1:8080/health | grep -F '\"status\":\"ok\"' >/dev/null"
            .to_string(),
    ];
    run_sprite_exec(sprite, org, &exec_args, &[])?;
    Ok(())
}

fn verify_agent_git_identity(sprite: &str, org: Option<&str>) -> Result<()> {
    let script = r#"set -euo pipefail
smoke_dir=/workspace/.git-identity-zodex-smoke
rm -rf "$smoke_dir"
git init -q "$smoke_dir"
cd "$smoke_dir"
printf "sprite git identity smoke\n" > smoke.txt
git add smoke.txt
git commit -q -m "Smoke: verify default agent git identity"
git log -1 --format="%an <%ae>"
cd /workspace
rm -rf "$smoke_dir"
"#;
    let exec_args = vec![
        "sudo".to_string(),
        "-u".to_string(),
        "zodex-agent".to_string(),
        "env".to_string(),
        "HOME=/home/zodex-agent".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
        script.to_string(),
    ];
    run_sprite_exec(sprite, org, &exec_args, &[])?;
    Ok(())
}

fn verify_reader_git_access(sprite: &str, org: Option<&str>, repo: &str) -> Result<()> {
    let exec_args = vec![
        "sudo".to_string(),
        "-u".to_string(),
        "zodex-agent".to_string(),
        "env".to_string(),
        "HOME=/home/zodex-agent".to_string(),
        "git".to_string(),
        "ls-remote".to_string(),
        format!("https://github.com/{repo}.git"),
        "HEAD".to_string(),
    ];
    run_sprite_exec(sprite, org, &exec_args, &[])?;
    Ok(())
}

fn verify_publisher_socket_permissions(sprite: &str, org: Option<&str>) -> Result<()> {
    let script = r#"set -euo pipefail
dir_path=/var/lib/zodex/publisher/run
sock_path=/var/lib/zodex/publisher/run/zodex-prd.sock
[[ "$(stat -c %a "$dir_path")" == "750" ]]
[[ "$(stat -c %U "$dir_path")" == "zodex-publisher" ]]
[[ "$(stat -c %G "$dir_path")" == "zodex" ]]
[[ "$(stat -c %a "$sock_path")" == "660" ]]
[[ "$(stat -c %U "$sock_path")" == "zodex-publisher" ]]
[[ "$(stat -c %G "$sock_path")" == "zodex" ]]
"#;
    let exec_args = vec![
        "sudo".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
        script.to_string(),
    ];
    run_sprite_exec(sprite, org, &exec_args, &[])?;
    Ok(())
}

fn verify_publisher_key_isolation(sprite: &str, org: Option<&str>) -> Result<()> {
    let script = r#"cat /etc/zodex/publisher/private-key.pem >/dev/null 2>&1"#;
    let exec_args = vec![
        "sudo".to_string(),
        "-u".to_string(),
        "zodex-agent".to_string(),
        "env".to_string(),
        "HOME=/home/zodex-agent".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
        script.to_string(),
    ];
    match run_sprite_exec(sprite, org, &exec_args, &[]) {
        Ok(_) => bail!(
            "zodex-agent unexpectedly gained read access to /etc/zodex/publisher/private-key.pem"
        ),
        Err(_) => Ok(()),
    }
}

fn verify_sprite_health(sprite: &str, org: Option<&str>, url_auth: Option<&str>) -> Result<()> {
    verify_local_sprite_health(sprite, org)?;
    if let Some(url_auth) = url_auth {
        set_sprite_url_auth(sprite, org, url_auth)?;
    }
    let info = sprite_url_info(sprite, org)?;
    if let Some(url) = info.url.as_deref() {
        if info.auth.as_deref() == Some("public") {
            run_command_capture(
                "curl",
                &[
                    "-fsS".to_string(),
                    "--retry".to_string(),
                    "3".to_string(),
                    "--retry-all-errors".to_string(),
                    "--retry-delay".to_string(),
                    "2".to_string(),
                    format!("{}/health", url.trim_end_matches('/')),
                ],
            )?;
        }
        println!("sprite-url: {url}");
        if let Some(host) = url
            .trim_end_matches('/')
            .strip_prefix("https://")
            .or_else(|| url.trim_end_matches('/').strip_prefix("http://"))
        {
            let exec_args = vec![
                "sudo".to_string(),
                AGENT_OPERATOR_BINARY.to_string(),
                "show-url".to_string(),
                "--host".to_string(),
                host.to_string(),
            ];
            let output = run_sprite_exec(sprite, org, &exec_args, &[])?;
            print!("{output}");
        }
    }
    println!("sprite-health: ok");
    Ok(())
}

async fn sprite_setup(options: SpriteSetupOptions<'_>) -> Result<()> {
    validate_sprite_url_auth(options.url_auth)?;
    let reader_installation_id =
        resolve_repo_installation_id(options.reader_app_id, options.reader_pem, options.repo)
            .await?;
    let publisher_installation_id = resolve_repo_installation_id(
        options.publisher_app_id,
        options.publisher_pem,
        options.repo,
    )
    .await?;
    mint_reader_installation_token(
        options.reader_app_id,
        options.reader_pem,
        reader_installation_id,
    )
    .await?;
    mint_publisher_installation_token_with_metadata(
        options.publisher_app_id,
        options.publisher_pem,
        publisher_installation_id,
    )
    .await?;

    let script = build_sprite_setup_script(
        options.repo,
        options.reader_app_id,
        reader_installation_id,
        options.publisher_app_id,
        publisher_installation_id,
        options.default_base,
        options.remote_config,
    );
    let mut script_file = NamedTempFile::new().context("failed to create setup temp file")?;
    use std::io::Write as _;
    script_file
        .write_all(script.as_bytes())
        .context("failed to write setup script")?;
    let exec_args = vec![
        "bash".to_string(),
        SPRITE_SETUP_REMOTE_SCRIPT_PATH.to_string(),
    ];
    run_sprite_exec(
        options.sprite,
        options.org,
        &exec_args,
        &[
            (script_file.path(), SPRITE_SETUP_REMOTE_SCRIPT_PATH),
            (options.reader_pem, "/tmp/zodex-reader.pem"),
            (options.publisher_pem, "/tmp/zodex-publisher.pem"),
        ],
    )?;

    sync_sprite_services(
        options.sprite,
        options.org,
        options.remote_config,
        true,
        false,
    )?;
    verify_publisher_socket_permissions(options.sprite, options.org)?;
    verify_sprite_service_logs(options.sprite, options.org)?;
    verify_sprite_health(options.sprite, options.org, Some(options.url_auth))?;
    if let Err(err) = register_operator_sprite(options.sprite, options.org, options.remote_config) {
        eprintln!("warning: failed to update local Sprite registry: {err:#}");
    }
    println!("sprite-setup: complete");
    Ok(())
}

fn sprite_upgrade(
    sprite: &str,
    org: Option<&str>,
    version: &str,
    repo: Option<&str>,
    url_auth: Option<&str>,
    remote_config: &Path,
) -> Result<()> {
    if let Some(url_auth) = url_auth {
        validate_sprite_url_auth(url_auth)?;
    }

    let repo_arg = repo.unwrap_or("");
    let script = build_sprite_upgrade_script(version, repo_arg, remote_config);
    let mut script_file = NamedTempFile::new().context("failed to create upgrade temp file")?;
    use std::io::Write as _;
    script_file
        .write_all(script.as_bytes())
        .context("failed to write upgrade script")?;

    let exec_args = vec![
        "bash".to_string(),
        SPRITE_UPGRADE_REMOTE_SCRIPT_PATH.to_string(),
    ];
    run_sprite_exec(
        sprite,
        org,
        &exec_args,
        &[(script_file.path(), SPRITE_UPGRADE_REMOTE_SCRIPT_PATH)],
    )?;

    sync_sprite_services(sprite, org, remote_config, false, false)?;
    verify_sprite_service_logs(sprite, org)?;
    verify_local_sprite_health(sprite, org)?;
    verify_agent_git_identity(sprite, org)?;
    if let Some(repo) =
        repo.map(str::to_string)
            .or(derive_remote_target_repo(sprite, org, remote_config)?)
    {
        verify_reader_git_access(sprite, org, &repo)?;
    }
    verify_publisher_socket_permissions(sprite, org)?;
    verify_publisher_key_isolation(sprite, org)?;
    verify_sprite_health(sprite, org, url_auth)?;
    if let Err(err) = register_operator_sprite(sprite, org, remote_config) {
        eprintln!("warning: failed to update local Sprite registry: {err:#}");
    }
    println!("sprite-upgrade: complete");
    Ok(())
}

fn build_sprite_setup_script(
    repo: &str,
    reader_app_id: u64,
    reader_installation_id: u64,
    publisher_app_id: u64,
    publisher_installation_id: u64,
    default_base: &str,
    remote_config: &Path,
) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

REPO={repo}
CFG={cfg}

if ! command -v git >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update -y
  sudo apt-get install -y --no-install-recommends git curl ca-certificates
fi

TMP_INSTALLER="$(mktemp)"
curl -fsSL https://zodex.ashray.xyz/install.sh -o "$TMP_INSTALLER"
sudo env \
  ZODEX_INSTALL_MODE=runtime \
  ZODEX_INSTALL_OPERATOR_CLI=0 \
  ZODEX_CONFIG_PATH="$CFG" \
  ZODEX_HTTP_BIND_PORT=8080 \
  ZODEX_AGENT_HOME=/home/zodex-agent \
  ZODEX_DEFAULT_WORKDIR=/workspace \
  bash "$TMP_INSTALLER"
rm -f "$TMP_INSTALLER"

sudo install -d -m 0750 -o root -g zodex /etc/zodex/reader /etc/zodex/publisher
sudo install -m 0640 -o root -g zodex /tmp/zodex-reader.pem /etc/zodex/reader/private-key.pem
sudo install -m 0600 -o zodex-publisher -g zodex /tmp/zodex-publisher.pem /etc/zodex/publisher/private-key.pem

sudo awk '
  BEGIN {{seen_bind=0; inserted_http=0}}
  /^bind_port = / {{
    print "bind_port = 8443"
    if (!inserted_http) {{
      print "http_bind_port = 8080"
      inserted_http=1
    }}
    seen_bind=1
    next
  }}
  /^http_bind_port = / {{next}}
  {{print}}
  END {{
    if (!seen_bind) {{
      print "bind_port = 8443"
      if (!inserted_http) {{
        print "http_bind_port = 8080"
      }}
    }}
  }}
' "$CFG" | sudo tee "$CFG" >/dev/null

sudo awk '
  BEGIN {{ seen_agent_home=0; seen_default_workdir=0 }}
  /^agent_home = / {{ print "agent_home = \"/home/zodex-agent\""; seen_agent_home=1; next }}
  /^default_workdir = / {{ print "default_workdir = \"/workspace\""; seen_default_workdir=1; next }}
  {{ print }}
  END {{
    if (!seen_agent_home) print "agent_home = \"/home/zodex-agent\""
    if (!seen_default_workdir) print "default_workdir = \"/workspace\""
  }}
' "$CFG" | sudo tee "$CFG" >/dev/null

tmp_cfg="$(mktemp)"
tmp_block="$(mktemp)"
sudo awk '
  BEGIN {{ skip=0 }}
  /^# BEGIN ZODEX_GH_APPS_MANAGED$/ {{ skip=1; next }}
  /^# END ZODEX_GH_APPS_MANAGED$/ {{ skip=0; next }}
  skip==0 {{ print }}
' "$CFG" > "$tmp_cfg"

cat > "$tmp_block" <<'EOF'
# BEGIN ZODEX_GH_APPS_MANAGED
reader_app_id = {reader_app_id}
reader_installation_id = {reader_installation_id}
publisher_app_id = {publisher_app_id}

[[publisher_targets]]
id = "{repo_plain}"
repo = "{repo_plain}"
default_base = "{default_base}"
installation_id = {publisher_installation_id}

[[publisher_installations]]
account = "{repo_account}"
default_base = "{default_base}"
installation_id = {publisher_installation_id}
# END ZODEX_GH_APPS_MANAGED
EOF

sudo bash -lc 'cat "$1" "$2" > "$3"' -- "$tmp_cfg" "$tmp_block" "$CFG"
rm -f "$tmp_cfg" "$tmp_block"
sudo chgrp zodex "$CFG"
sudo chmod 0640 "$CFG"

helper_cmd="/usr/local/bin/zodex-agent --config $CFG git-credential-helper"
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global --replace-all credential.https://github.com.helper "$helper_cmd"
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global credential.https://github.com.useHttpPath true
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global url."zodex::https://github.com/".pushInsteadOf https://github.com/
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global user.name "Zodex Agent"
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global user.email "zodex-agent@local.invalid"

sudo -u zodex-agent env HOME=/home/zodex-agent bash -lc '
  cd /workspace
  test -w /workspace
  touch .zodex-write-check
  rm -f .zodex-write-check
'

sudo -u zodex-agent env HOME=/home/zodex-agent bash -lc '
  smoke_dir=/workspace/.git-identity-smoke
  rm -rf "$smoke_dir"
  git init -q "$smoke_dir"
  cd "$smoke_dir"
  printf "sprite git identity smoke\n" > smoke.txt
  git add smoke.txt
  git commit -q -m "Smoke: verify default agent git identity"
  cd /workspace
  rm -rf "$smoke_dir"
'

sudo -u zodex-agent env HOME=/home/zodex-agent \
  git -C /workspace ls-remote "https://github.com/$REPO.git" HEAD >/dev/null

if sudo -u zodex-agent env HOME=/home/zodex-agent \
  bash -lc 'cat /etc/zodex/publisher/private-key.pem >/dev/null 2>&1'; then
  echo "agent unexpectedly gained publisher key access" >&2
  exit 1
fi

sudo bash -lc 'pkill -f -- "/usr/local/bin/zodexd --config $1" || true; pkill -f -- "/usr/local/bin/zodex-prd --config $1" || true' -- "$CFG"
rm -f /tmp/zodex-reader.pem /tmp/zodex-publisher.pem {setup_script}
"#,
        repo = shell_escape_single_quotes(repo),
        repo_plain = repo,
        cfg = shell_escape_single_quotes(&remote_config.display().to_string()),
        reader_app_id = reader_app_id,
        reader_installation_id = reader_installation_id,
        publisher_app_id = publisher_app_id,
        publisher_installation_id = publisher_installation_id,
        default_base = default_base,
        setup_script = SPRITE_SETUP_REMOTE_SCRIPT_PATH,
        repo_account = repo.split('/').next().unwrap_or(repo)
    )
}

fn build_sprite_upgrade_script(version: &str, repo: &str, remote_config: &Path) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

CFG={cfg}
VERSION={version}
TARGET_REPO={repo}
INSTALLER_REF="$VERSION"
if [[ "$VERSION" == "latest" ]]; then
  INSTALLER_REF="main"
fi

if [[ ! -f "$CFG" ]]; then
  echo "missing $CFG" >&2
  exit 1
fi

if ! command -v git >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update -y
  sudo apt-get install -y --no-install-recommends git curl ca-certificates
fi

HTTP_BIND_PORT="$(sudo awk -F'= ' '/^http_bind_port = / {{ print $2; exit }}' "$CFG" 2>/dev/null || true)"
REPO_FOR_INSTALL="amxv/zodex"
if [[ -n "$TARGET_REPO" ]]; then
  REPO_FOR_INSTALL="$TARGET_REPO"
fi

INSTALLER_URL="https://raw.githubusercontent.com/${{REPO_FOR_INSTALL}}/${{INSTALLER_REF}}/scripts/install.sh"
TMP_INSTALLER="$(mktemp)"
curl -fsSL "$INSTALLER_URL" -o "$TMP_INSTALLER"
sudo env \
  ZODEX_REPO="$REPO_FOR_INSTALL" \
  ZODEX_VERSION="$VERSION" \
  ZODEX_SOURCE_REF="$VERSION" \
  ZODEX_INSTALL_OPERATOR_CLI=0 \
  ZODEX_CONFIG_PATH="$CFG" \
  ZODEX_HTTP_BIND_PORT="$HTTP_BIND_PORT" \
  bash "$TMP_INSTALLER"
rm -f "$TMP_INSTALLER"

if [[ -z "$TARGET_REPO" ]]; then
  TARGET_REPO="$(sudo awk -F'"' '/^\[\[publisher_targets\]\]/ {{ in_targets=1; next }} in_targets && /^repo = "/ {{ print $2; exit }}' "$CFG" 2>/dev/null || true)"
fi

helper_cmd="/usr/local/bin/zodex-agent --config $CFG git-credential-helper"
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global --replace-all credential.https://github.com.helper "$helper_cmd"
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global credential.https://github.com.useHttpPath true
sudo -u zodex-agent env HOME=/home/zodex-agent git config --global url."zodex::https://github.com/".pushInsteadOf https://github.com/

current_name="$(sudo -u zodex-agent env HOME=/home/zodex-agent git config --global --get user.name || true)"
current_email="$(sudo -u zodex-agent env HOME=/home/zodex-agent git config --global --get user.email || true)"
if [[ -z "$current_name" ]]; then
  sudo -u zodex-agent env HOME=/home/zodex-agent git config --global user.name "Zodex Agent"
fi
if [[ -z "$current_email" ]]; then
  sudo -u zodex-agent env HOME=/home/zodex-agent git config --global user.email "zodex-agent@local.invalid"
fi

rm -f {upgrade_script}
"#,
        cfg = shell_escape_single_quotes(&remote_config.display().to_string()),
        version = shell_escape_single_quotes(version),
        repo = shell_escape_single_quotes(repo),
        upgrade_script = SPRITE_UPGRADE_REMOTE_SCRIPT_PATH
    )
}
