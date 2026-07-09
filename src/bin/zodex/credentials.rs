#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct GitCredentialRequest {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    url: Option<String>,
    username: Option<String>,
}

async fn handle_git_credential_helper(config: &Config, operation: &str) -> Result<()> {
    let request = read_git_credential_request()?;

    if operation != "get" || !git_credential_request_targets_github(&request) {
        return Ok(());
    }

    if let Some(grant) = load_matching_push_grant(&request, Path::new(PUSH_GRANTS_DIR))? {
        println!("username=x-access-token");
        println!("password={}", grant.token);
        println!();
        return Ok(());
    }

    ensure_reader_ready_for_start(config)?;
    let token = mint_reader_installation_token(
        config.reader_app_id.unwrap_or_default(),
        Path::new(&config.reader_private_key_path),
        config.reader_installation_id.unwrap_or_default(),
    )
    .await?;

    println!("username=x-access-token");
    println!("password={token}");
    println!();
    Ok(())
}

fn read_git_credential_request() -> Result<GitCredentialRequest> {
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .context("failed to read git credential request from stdin")?;
    Ok(parse_git_credential_request(&raw))
}

fn parse_git_credential_request(raw: &str) -> GitCredentialRequest {
    let mut request = GitCredentialRequest::default();

    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "protocol" => request.protocol = Some(value.to_string()),
            "host" => request.host = Some(value.to_string()),
            "path" => request.path = Some(value.to_string()),
            "url" => request.url = Some(value.to_string()),
            "username" => request.username = Some(value.to_string()),
            _ => {}
        }
    }

    request
}

fn git_credential_request_targets_github(request: &GitCredentialRequest) -> bool {
    let protocol = request
        .protocol
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_protocol));
    let host = request
        .host
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_host));

    matches!(protocol, Some(protocol) if protocol.eq_ignore_ascii_case("https"))
        && matches!(host, Some(host) if credential_host_is_github(host))
}

fn credential_url_protocol(url: &str) -> Option<&str> {
    url.split_once("://").map(|(scheme, _)| scheme)
}

fn credential_url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    Some(host.split('@').next_back().unwrap_or(host))
}

fn credential_url_path(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let (_, path) = rest.split_once('/')?;
    Some(path)
}

fn credential_host_is_github(host: &str) -> bool {
    let normalized = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    normalized == "github.com" || normalized == "www.github.com"
}

fn git_credential_request_repo(request: &GitCredentialRequest) -> Option<String> {
    let path = request
        .path
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_path))?;
    normalize_github_repo(path)
}

fn normalize_github_repo(path: &str) -> Option<String> {
    let trimmed = path.trim_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut parts = trimmed.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn normalize_github_repos(repos: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for repo in repos {
        let repo = normalize_github_repo(repo)
            .ok_or_else(|| anyhow!("repo must be in owner/repo form: {repo}"))?;
        if !normalized.contains(&repo) {
            normalized.push(repo);
        }
    }
    Ok(normalized)
}

fn operator_sprites_registry_path_from_home(home: &Path) -> PathBuf {
    home.join(OPERATOR_SPRITES_REGISTRY_RELATIVE_PATH)
}

fn operator_sprites_registry_path() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME must be set to use the zodex Sprite registry")?;
    Ok(operator_sprites_registry_path_from_home(Path::new(&home)))
}

fn load_operator_sprite_registry_from_path(path: &Path) -> Result<OperatorSpriteRegistry> {
    if !path.exists() {
        return Ok(OperatorSpriteRegistry::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read Sprite registry at {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse Sprite registry at {}", path.display()))
}

fn save_operator_sprite_registry_to_path(
    path: &Path,
    registry: &OperatorSpriteRegistry,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(registry).context("failed to encode Sprite registry")?;
    fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn upsert_operator_sprite_record(
    registry: &mut OperatorSpriteRegistry,
    record: OperatorSpriteRecord,
) {
    if let Some(existing) = registry
        .sprites
        .iter_mut()
        .find(|candidate| candidate.name == record.name && candidate.org == record.org)
    {
        *existing = record;
    } else {
        registry.sprites.push(record);
    }
    registry
        .sprites
        .sort_by(|a, b| (&a.org, &a.name).cmp(&(&b.org, &b.name)));
}

fn register_operator_sprite(sprite: &str, org: Option<&str>, remote_config: &Path) -> Result<()> {
    let path = operator_sprites_registry_path()?;
    let mut registry = load_operator_sprite_registry_from_path(&path)?;
    let record = OperatorSpriteRecord {
        name: sprite.to_string(),
        org: org.map(str::to_string),
        remote_config: remote_config.display().to_string(),
        last_setup_at: format_epoch_seconds_rfc3339(current_epoch_seconds()?)?,
    };
    upsert_operator_sprite_record(&mut registry, record);
    save_operator_sprite_registry_to_path(&path, &registry)
}

fn resolve_remote_sprite_from_registry(
    explicit_sprite: Option<&str>,
    explicit_org: Option<&str>,
    env_sprite: Option<&str>,
    registry: &OperatorSpriteRegistry,
) -> Result<ResolvedSprite> {
    if let Some(sprite) = explicit_sprite {
        return Ok(ResolvedSprite {
            name: sprite.to_string(),
            org: explicit_org.map(str::to_string),
        });
    }
    if let Some(sprite) = env_sprite.filter(|value| !value.trim().is_empty()) {
        return Ok(ResolvedSprite {
            name: sprite.to_string(),
            org: explicit_org.map(str::to_string),
        });
    }

    let candidates: Vec<&OperatorSpriteRecord> = registry
        .sprites
        .iter()
        .filter(|candidate| match explicit_org {
            Some(org) => candidate.org.as_deref() == Some(org),
            None => true,
        })
        .collect();

    match candidates.as_slice() {
        [candidate] => Ok(ResolvedSprite {
            name: candidate.name.clone(),
            org: candidate.org.clone(),
        }),
        [] => bail!(
            "pass `--sprite <name>`, set `ZODEX_SPRITE`, or run `zodex sprite setup` once to register a default Sprite"
        ),
        many => {
            let names = many
                .iter()
                .map(|candidate| match candidate.org.as_deref() {
                    Some(org) => format!("{}/{}", org, candidate.name),
                    None => candidate.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "multiple Sprites are configured ({names}); pass `--sprite <name>` or set `ZODEX_SPRITE`"
            )
        }
    }
}

fn resolve_remote_sprite(sprite: Option<&str>, org: Option<&str>) -> Result<ResolvedSprite> {
    let env_sprite = env::var(ZODEX_SPRITE_ENV).ok();
    let registry = load_operator_sprite_registry_from_path(&operator_sprites_registry_path()?)?;
    resolve_remote_sprite_from_registry(sprite, org, env_sprite.as_deref(), &registry)
}

fn push_grant_file_name(repo: &str) -> String {
    format!("{}.json", repo.replace('/', "__"))
}

fn push_grant_path(repo: &str) -> PathBuf {
    Path::new(PUSH_GRANTS_DIR).join(push_grant_file_name(repo))
}

fn push_grant_expired(grant: &PushGrantRecord, now_epoch_seconds: u64) -> bool {
    matches!(
        grant.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn current_epoch_seconds() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn format_epoch_seconds_rfc3339(epoch_seconds: u64) -> Result<String> {
    OffsetDateTime::from_unix_timestamp(epoch_seconds as i64)
        .context("failed to build RFC3339 timestamp from epoch seconds")?
        .format(&Rfc3339)
        .context("failed to format RFC3339 timestamp")
}

fn month_name(month: time::Month) -> &'static str {
    match month {
        time::Month::January => "January",
        time::Month::February => "February",
        time::Month::March => "March",
        time::Month::April => "April",
        time::Month::May => "May",
        time::Month::June => "June",
        time::Month::July => "July",
        time::Month::August => "August",
        time::Month::September => "September",
        time::Month::October => "October",
        time::Month::November => "November",
        time::Month::December => "December",
    }
}

fn local_offset_label(offset: UtcOffset) -> String {
    let total_seconds = offset.whole_seconds();
    if total_seconds == 0 {
        return "UTC".to_string();
    }
    if total_seconds == 5 * 60 * 60 + 30 * 60 {
        return "IST".to_string();
    }

    let sign = if total_seconds >= 0 { '+' } else { '-' };
    let absolute_seconds = total_seconds.unsigned_abs();
    let hours = absolute_seconds / 3_600;
    let minutes = (absolute_seconds % 3_600) / 60;
    format!("UTC{sign}{hours:02}:{minutes:02}")
}

fn format_epoch_seconds_local_display(epoch_seconds: u64) -> Result<String> {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let local = OffsetDateTime::from_unix_timestamp(epoch_seconds as i64)
        .context("failed to build local display timestamp from epoch seconds")?
        .to_offset(offset);
    let hour_24 = local.hour();
    let display_hour = match hour_24 % 12 {
        0 => 12,
        hour => hour,
    };
    let meridiem = if hour_24 < 12 { "AM" } else { "PM" };
    Ok(format!(
        "{} {} {} {}:{:02} {} {}",
        local.day(),
        month_name(local.month()),
        local.year(),
        display_hour,
        local.minute(),
        meridiem,
        local_offset_label(offset)
    ))
}

fn expires_at_from_now(expires_in_seconds: u64) -> Result<(String, u64)> {
    let expires_at_epoch_seconds = current_epoch_seconds()?
        .checked_add(expires_in_seconds)
        .ok_or_else(|| anyhow!("push grant expiration overflowed"))?;
    Ok((
        format_epoch_seconds_rfc3339(expires_at_epoch_seconds)?,
        expires_at_epoch_seconds,
    ))
}

fn parse_push_grant_ttl(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("push grant TTL must not be empty");
    }
    let unit = trimmed
        .chars()
        .last()
        .ok_or_else(|| anyhow!("push grant TTL must not be empty"))?;
    let (value_part, multiplier_seconds) = if unit.is_ascii_alphabetic() {
        let value = &trimmed[..trimmed.len() - unit.len_utf8()];
        let multiplier = match unit {
            's' | 'S' => 1,
            'm' | 'M' => 60,
            'h' | 'H' => 60 * 60,
            'd' | 'D' => 60 * 60 * 24,
            _ => bail!("unsupported push grant TTL unit `{unit}`; use s, m, h, or d"),
        };
        (value, multiplier)
    } else {
        (trimmed, 1)
    };
    let amount = value_part
        .parse::<u64>()
        .with_context(|| format!("failed to parse push grant TTL `{raw}`"))?;
    if amount == 0 {
        bail!("push grant TTL must be greater than zero");
    }
    let seconds = amount
        .checked_mul(multiplier_seconds)
        .ok_or_else(|| anyhow!("push grant TTL is too large"))?;
    Ok(Duration::from_secs(seconds))
}

fn write_local_push_grant(repo: &str, grant: &PushGrantRecord) -> Result<()> {
    let path = push_grant_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(grant).context("failed to encode push grant")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn load_push_grant_from_dir(repo: &str, grants_dir: &Path) -> Result<Option<PushGrantRecord>> {
    let path = grants_dir.join(push_grant_file_name(repo));
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let grant: PushGrantRecord = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if push_grant_expired(&grant, current_epoch_seconds()?) {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(grant))
}

fn load_matching_push_grant(
    request: &GitCredentialRequest,
    grants_dir: &Path,
) -> Result<Option<PushGrantRecord>> {
    let Some(repo) = git_credential_request_repo(request) else {
        return Ok(None);
    };
    load_push_grant_from_dir(&repo, grants_dir)
}

fn parse_push_grants(raw: &str) -> Result<Vec<PushGrantRecord>> {
    serde_json::Deserializer::from_str(raw)
        .into_iter::<PushGrantRecord>()
        .map(|grant| grant.context("failed to parse push grant"))
        .collect()
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[allow(dead_code)]
fn resolve_local_operator_binaries() -> Result<LocalOperatorBinaries> {
    let agent_cli_candidates = [
        manifest_dir().join("target/debug/zodex-agent"),
        manifest_dir().join("target/release/zodex-agent"),
        PathBuf::from("/usr/local/bin/zodex-agent"),
    ];
    let git_remote_helper_candidates = [
        manifest_dir().join("target/debug/git-remote-zodex"),
        manifest_dir().join("target/release/git-remote-zodex"),
        PathBuf::from("/usr/local/bin/git-remote-zodex"),
    ];
    let daemon_candidates = [
        manifest_dir().join("target/debug/zodexd"),
        manifest_dir().join("target/release/zodexd"),
        manifest_dir().join("target/debug/zodexd"),
        manifest_dir().join("target/release/zodexd"),
        PathBuf::from("/usr/local/bin/zodexd"),
        PathBuf::from("/usr/local/bin/zodexd"),
    ];
    let publisher_candidates = [
        manifest_dir().join("target/debug/zodex-prd"),
        manifest_dir().join("target/release/zodex-prd"),
        PathBuf::from("/usr/local/bin/zodex-prd"),
    ];

    let mut agent_cli = first_existing_executable(&agent_cli_candidates);
    let mut git_remote_helper = first_existing_executable(&git_remote_helper_candidates);
    let mut daemon = first_existing_executable(&daemon_candidates);
    let mut publisher = first_existing_executable(&publisher_candidates);

    if agent_cli.is_none() || git_remote_helper.is_none() || daemon.is_none() || publisher.is_none()
    {
        build_local_operator_binaries()?;
        agent_cli = first_existing_executable(&agent_cli_candidates);
        git_remote_helper = first_existing_executable(&git_remote_helper_candidates);
        daemon = first_existing_executable(&daemon_candidates);
        publisher = first_existing_executable(&publisher_candidates);
    }

    match (agent_cli, git_remote_helper, daemon, publisher) {
        (Some(agent_cli), Some(git_remote_helper), Some(daemon), Some(publisher)) => {
            Ok(LocalOperatorBinaries {
                agent_cli,
                git_remote_helper,
                daemon,
                publisher,
            })
        }
        _ => bail!(
            "failed to locate local zodex runtime binaries; expected zodex-agent, git-remote-zodex, zodexd, and zodex-prd"
        ),
    }
}

#[allow(dead_code)]
fn first_existing_executable(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

#[allow(dead_code)]
fn build_local_operator_binaries() -> Result<()> {
    let args = vec![
        "build".to_string(),
        "--bin".to_string(),
        "zodex-agent".to_string(),
        "--bin".to_string(),
        "git-remote-zodex".to_string(),
        "--bin".to_string(),
        "zodexd".to_string(),
        "--bin".to_string(),
        "zodex-prd".to_string(),
    ];
    run_command_capture("cargo", &args)
        .context("failed to build local zodex binaries")
        .map(|_| ())
}
