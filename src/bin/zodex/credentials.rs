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

#[cfg(not(target_os = "windows"))]
fn operator_sprites_registry_path_from_home(home: &Path) -> PathBuf {
    home.join(OPERATOR_SPRITES_REGISTRY_RELATIVE_PATH)
}

fn operator_sprites_registry_path() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(app_data).join("zodex/sprites.json"));
        }
        let profile = env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .context(
                "APPDATA or USERPROFILE must be set to use the zodex Sprite registry on Windows",
            )?;
        return Ok(PathBuf::from(profile).join("AppData/Roaming/zodex/sprites.json"));
    }

    #[cfg(not(target_os = "windows"))]
    {
    let home = env::var("HOME").context("HOME must be set to use the zodex Sprite registry")?;
    Ok(operator_sprites_registry_path_from_home(Path::new(&home)))
    }
}

fn load_operator_sprite_registry_from_path(path: &Path) -> Result<OperatorSpriteRegistry> {
    if !path.exists() {
        return Ok(OperatorSpriteRegistry::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read Sprite registry at {}", path.display()))?;
    let mut registry: OperatorSpriteRegistry = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse Sprite registry at {}", path.display()))?;
    match registry.version {
        1 => registry.version = OPERATOR_SPRITES_REGISTRY_VERSION,
        OPERATOR_SPRITES_REGISTRY_VERSION => {}
        other => bail!(
            "unsupported Sprite registry version {other} at {}; expected <= {}",
            path.display(),
            OPERATOR_SPRITES_REGISTRY_VERSION
        ),
    }
    Ok(registry)
}

fn save_operator_sprite_registry_to_path(
    path: &Path,
    registry: &OperatorSpriteRegistry,
) -> Result<()> {
    if registry.version != OPERATOR_SPRITES_REGISTRY_VERSION {
        bail!(
            "refusing to write Sprite registry version {}; expected {}",
            registry.version,
            OPERATOR_SPRITES_REGISTRY_VERSION
        );
    }
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
    let setup_at = format_epoch_seconds_rfc3339(current_epoch_seconds()?)?;
    update_operator_sprite_setup_record(&mut registry, sprite, org, remote_config, &setup_at);
    save_operator_sprite_registry_to_path(&path, &registry)
}

fn update_operator_sprite_setup_record(
    registry: &mut OperatorSpriteRegistry,
    sprite: &str,
    org: Option<&str>,
    remote_config: &Path,
    setup_at: &str,
) {
    let existing_proxy = registry
        .sprites
        .iter()
        .find(|candidate| candidate.name == sprite && candidate.org.as_deref() == org)
        .and_then(|candidate| candidate.proxy.clone());
    let record = OperatorSpriteRecord {
        name: sprite.to_string(),
        org: org.map(str::to_string),
        remote_config: remote_config.display().to_string(),
        last_setup_at: setup_at.to_string(),
        proxy: existing_proxy,
    };
    upsert_operator_sprite_record(registry, record);
}

fn operator_sprite_record<'a>(
    registry: &'a OperatorSpriteRegistry,
    sprite: &ResolvedSprite,
) -> Option<&'a OperatorSpriteRecord> {
    registry.sprites.iter().find(|candidate| {
        candidate.name == sprite.name && candidate.org.as_deref() == sprite.org.as_deref()
    })
}

fn operator_sprite_record_mut<'a>(
    registry: &'a mut OperatorSpriteRegistry,
    sprite: &ResolvedSprite,
) -> Option<&'a mut OperatorSpriteRecord> {
    registry.sprites.iter_mut().find(|candidate| {
        candidate.name == sprite.name && candidate.org.as_deref() == sprite.org.as_deref()
    })
}

fn load_operator_sprite_record(sprite: &ResolvedSprite) -> Result<Option<OperatorSpriteRecord>> {
    let registry = load_operator_sprite_registry_from_path(&operator_sprites_registry_path()?)?;
    Ok(operator_sprite_record(&registry, sprite).cloned())
}

fn save_operator_sprite_proxy_record(
    sprite: &ResolvedSprite,
    proxy: OperatorSpriteProxyRecord,
) -> Result<()> {
    let path = operator_sprites_registry_path()?;
    let mut registry = load_operator_sprite_registry_from_path(&path)?;
    update_operator_sprite_proxy_record(&mut registry, sprite, proxy);
    save_operator_sprite_registry_to_path(&path, &registry)
}

fn update_operator_sprite_proxy_record(
    registry: &mut OperatorSpriteRegistry,
    sprite: &ResolvedSprite,
    proxy: OperatorSpriteProxyRecord,
) {
    if let Some(record) = operator_sprite_record_mut(registry, sprite) {
        record.proxy = Some(proxy);
    } else {
        registry.sprites.push(OperatorSpriteRecord {
            name: sprite.name.clone(),
            org: sprite.org.clone(),
            remote_config: DEFAULT_CONFIG_PATH.to_string(),
            last_setup_at: String::new(),
            proxy: Some(proxy),
        });
        registry
            .sprites
            .sort_by(|a, b| (&a.org, &a.name).cmp(&(&b.org, &b.name)));
    }
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
