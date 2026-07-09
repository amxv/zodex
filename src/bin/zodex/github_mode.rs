fn github_yolo_expiration_from(
    created_at_epoch_seconds: u64,
    active_ttl: Option<Duration>,
) -> Result<(Option<String>, Option<u64>)> {
    match active_ttl {
        Some(active_ttl) => {
            let expires_at_epoch_seconds = created_at_epoch_seconds
                .checked_add(active_ttl.as_secs())
                .ok_or_else(|| anyhow!("YOLO mode expiration overflowed"))?;
            Ok((
                Some(format_epoch_seconds_rfc3339(expires_at_epoch_seconds)?),
                Some(expires_at_epoch_seconds),
            ))
        }
        None => Ok((None, None)),
    }
}

fn build_github_yolo_mode_record(
    repos: &[String],
    active_ttl: Option<Duration>,
) -> Result<GithubModeRecord> {
    let repos = normalize_github_repos(repos)?;
    let created_at_epoch_seconds = current_epoch_seconds()?;
    build_github_yolo_mode_record_at(&repos, active_ttl, created_at_epoch_seconds)
}

fn build_github_yolo_mode_record_at(
    repos: &[String],
    active_ttl: Option<Duration>,
    created_at_epoch_seconds: u64,
) -> Result<GithubModeRecord> {
    let created_at = format_epoch_seconds_rfc3339(created_at_epoch_seconds)?;
    let (expires_at, expires_at_epoch_seconds) =
        github_yolo_expiration_from(created_at_epoch_seconds, active_ttl)?;
    let repo_grants = repos
        .iter()
        .map(|repo| GithubYoloRepoGrant {
            repo: repo.clone(),
            created_at: created_at.clone(),
            expires_at: expires_at.clone(),
            expires_at_epoch_seconds,
        })
        .collect();
    Ok(GithubModeRecord {
        mode: "yolo".to_string(),
        all_installed: repos.is_empty(),
        repos: repos.to_vec(),
        repo_grants,
        created_at,
        expires_at,
        expires_at_epoch_seconds,
        enabled_by: "operator-cli".to_string(),
        token_source: "publisher-app-installation-token".to_string(),
    })
}

fn github_yolo_repo_grant_expired(grant: &GithubYoloRepoGrant, now_epoch_seconds: u64) -> bool {
    matches!(
        grant.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn github_mode_expired(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    !github_mode_has_active_scope(record, now_epoch_seconds)
}

fn github_yolo_all_installed_active(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    if !record.all_installed {
        return false;
    }
    !matches!(
        record.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn github_yolo_active_repo_grants(
    record: &GithubModeRecord,
    now_epoch_seconds: u64,
) -> Vec<GithubYoloRepoGrant> {
    if !record.repo_grants.is_empty() {
        return record
            .repo_grants
            .iter()
            .filter(|grant| !github_yolo_repo_grant_expired(grant, now_epoch_seconds))
            .cloned()
            .collect();
    }

    if record.all_installed
        || matches!(
            record.expires_at_epoch_seconds,
            Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
        )
    {
        return Vec::new();
    }

    record
        .repos
        .iter()
        .map(|repo| GithubYoloRepoGrant {
            repo: repo.clone(),
            created_at: record.created_at.clone(),
            expires_at: record.expires_at.clone(),
            expires_at_epoch_seconds: record.expires_at_epoch_seconds,
        })
        .collect()
}

fn github_mode_has_active_scope(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    github_yolo_all_installed_active(record, now_epoch_seconds)
        || !github_yolo_active_repo_grants(record, now_epoch_seconds).is_empty()
}

fn merge_github_yolo_mode_records(
    existing: Option<GithubModeRecord>,
    next: GithubModeRecord,
    now_epoch_seconds: u64,
) -> GithubModeRecord {
    let mut merged = match existing {
        Some(existing)
            if existing.mode == "yolo" && !github_mode_expired(&existing, now_epoch_seconds) =>
        {
            existing
        }
        _ => return next,
    };

    let mut active_repo_grants = github_yolo_active_repo_grants(&merged, now_epoch_seconds);
    for grant in next.repo_grants {
        if let Some(existing_grant) = active_repo_grants
            .iter_mut()
            .find(|existing_grant| existing_grant.repo == grant.repo)
        {
            *existing_grant = grant;
        } else {
            active_repo_grants.push(grant);
        }
    }
    active_repo_grants.sort_by(|left, right| left.repo.cmp(&right.repo));

    if next.all_installed {
        merged.all_installed = true;
        merged.created_at = next.created_at;
        merged.expires_at = next.expires_at;
        merged.expires_at_epoch_seconds = next.expires_at_epoch_seconds;
        merged.enabled_by = next.enabled_by;
        merged.token_source = next.token_source;
    } else if !github_yolo_all_installed_active(&merged, now_epoch_seconds) {
        merged.all_installed = false;
        merged.expires_at = None;
        merged.expires_at_epoch_seconds = None;
    }

    merged.repo_grants = active_repo_grants;
    merged.repos = merged
        .repo_grants
        .iter()
        .map(|grant| grant.repo.clone())
        .collect();
    merged
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GithubYoloAgentGitStatus {
    helper: String,
    use_http_path: String,
    push_rewrite: String,
}

impl GithubYoloAgentGitStatus {
    fn helper_ok(&self) -> bool {
        self.helper == expected_zodex_agent_git_helper()
    }

    fn use_http_path_ok(&self) -> bool {
        self.use_http_path.eq_ignore_ascii_case("true")
    }

    fn push_rewrite_ok(&self) -> bool {
        self.push_rewrite
            .lines()
            .any(|value| value == GITHUB_PUSH_REWRITE_SOURCE)
    }

    fn direct_push_ready(&self) -> bool {
        self.helper_ok() && self.use_http_path_ok() && self.push_rewrite_ok()
    }
}

fn expected_zodex_agent_git_helper() -> String {
    format!("{ZODEX_AGENT_BINARY_PATH} --config {DEFAULT_CONFIG_PATH} git-credential-helper")
}

fn github_yolo_agent_git_repair_script() -> String {
    let helper = expected_zodex_agent_git_helper();
    format!(
        r#"helper_cmd={helper:?}
sudo -u {user} env HOME={home:?} git config --global --replace-all credential.https://github.com.helper "$helper_cmd"
sudo -u {user} env HOME={home:?} git config --global credential.https://github.com.useHttpPath true
sudo -u {user} env HOME={home:?} git config --global --replace-all url.{rewrite_target:?}.pushInsteadOf {rewrite_source:?}
"#,
        user = ZODEX_AGENT_USER,
        home = ZODEX_AGENT_HOME,
        rewrite_target = GITHUB_PUSH_REWRITE_TARGET,
        rewrite_source = GITHUB_PUSH_REWRITE_SOURCE,
    )
}

fn github_yolo_agent_git_inspect_script() -> String {
    format!(
        r#"helper="$(sudo -u {user} env HOME={home:?} git config --global --get credential.https://github.com.helper || true)"
use_http_path="$(sudo -u {user} env HOME={home:?} git config --global --get credential.https://github.com.useHttpPath || true)"
push_rewrite="$(sudo -u {user} env HOME={home:?} git config --global --get-all url.{rewrite_target:?}.pushInsteadOf || true)"
printf 'helper=%s\n' "$helper"
printf 'use_http_path=%s\n' "$use_http_path"
printf 'push_rewrite=%s\n' "$push_rewrite"
"#,
        user = ZODEX_AGENT_USER,
        home = ZODEX_AGENT_HOME,
        rewrite_target = GITHUB_PUSH_REWRITE_TARGET,
    )
}

fn parse_github_yolo_agent_git_status(raw: &str) -> GithubYoloAgentGitStatus {
    let mut status = GithubYoloAgentGitStatus::default();
    let mut current_key = None;
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            if matches!(current_key, Some("push_rewrite")) {
                if !status.push_rewrite.is_empty() {
                    status.push_rewrite.push('\n');
                }
                status.push_rewrite.push_str(line);
            }
            continue;
        };
        match key {
            "helper" => {
                status.helper = value.to_string();
                current_key = Some("helper");
            }
            "use_http_path" => {
                status.use_http_path = value.to_string();
                current_key = Some("use_http_path");
            }
            "push_rewrite" => {
                status.push_rewrite = value.to_string();
                current_key = Some("push_rewrite");
            }
            _ => {}
        }
    }
    status
}

fn inspect_github_yolo_agent_git_status(
    resolved: &ResolvedSprite,
) -> Result<GithubYoloAgentGitStatus> {
    let exec_args = vec![
        "bash".to_string(),
        "-lc".to_string(),
        github_yolo_agent_git_inspect_script(),
    ];
    let raw = run_sprite_exec(&resolved.name, resolved.org.as_deref(), &exec_args, &[])?;
    Ok(parse_github_yolo_agent_git_status(&raw))
}

fn build_github_yolo_agent_git_status_lines(status: &GithubYoloAgentGitStatus) -> Vec<String> {
    vec![
        format!(
            "agent-git-helper: {}",
            if status.helper_ok() {
                "ok"
            } else {
                "missing-or-mismatched"
            }
        ),
        format!(
            "agent-git-use-http-path: {}",
            if status.use_http_path_ok() {
                "ok"
            } else {
                "missing-or-mismatched"
            }
        ),
        format!(
            "agent-git-push-rewrite: {}",
            if status.push_rewrite_ok() {
                "ok"
            } else {
                "missing"
            }
        ),
    ]
}

fn print_github_yolo_agent_git_status(status: &GithubYoloAgentGitStatus) {
    for line in build_github_yolo_agent_git_status_lines(status) {
        println!("{line}");
    }
}

fn print_epoch_local_line(label: &str, epoch_seconds: Option<u64>) {
    match epoch_seconds {
        Some(epoch_seconds) => match format_epoch_seconds_local_display(epoch_seconds) {
            Ok(local_display) => println!("{label}: {local_display}"),
            Err(_) => println!("{label}: unavailable"),
        },
        None => println!("{label}: none"),
    }
}

fn print_github_yolo_scope(record: &GithubModeRecord, now_epoch_seconds: u64) {
    if github_yolo_all_installed_active(record, now_epoch_seconds) {
        println!("scope: all-installed");
        if let Some(expires_at) = record.expires_at.as_deref() {
            println!("expires-at: {expires_at}");
        } else {
            println!("expires-at: none");
        }
        print_epoch_local_line("expires-at-local", record.expires_at_epoch_seconds);
    }

    let active_repo_grants = github_yolo_active_repo_grants(record, now_epoch_seconds);
    if !active_repo_grants.is_empty() {
        println!("scope: repo-allowlist");
        for grant in active_repo_grants {
            println!("repo: {}", grant.repo);
            if let Some(expires_at) = grant.expires_at.as_deref() {
                println!("repo-expires-at: {} {}", grant.repo, expires_at);
            } else {
                println!("repo-expires-at: {} none", grant.repo);
            }
            match grant.expires_at_epoch_seconds {
                Some(epoch_seconds) => match format_epoch_seconds_local_display(epoch_seconds) {
                    Ok(local_display) => {
                        println!("repo-expires-at-local: {} {}", grant.repo, local_display)
                    }
                    Err(_) => println!("repo-expires-at-local: {} unavailable", grant.repo),
                },
                None => println!("repo-expires-at-local: {} none", grant.repo),
            }
        }
    }
}

fn enable_github_yolo_mode(
    resolved: &ResolvedSprite,
    repos: &[String],
    active_ttl: Option<Duration>,
) -> Result<()> {
    let next_record = build_github_yolo_mode_record(repos, active_ttl)?;
    let existing_raw = run_sprite_exec(
        &resolved.name,
        resolved.org.as_deref(),
        &[
            "bash".to_string(),
            "-lc".to_string(),
            format!(
                "if sudo test -f {GITHUB_MODE_STATE_PATH}; then sudo cat {GITHUB_MODE_STATE_PATH}; fi"
            ),
        ],
        &[],
    )?;
    let existing_record = if existing_raw.trim().is_empty() {
        None
    } else {
        Some(
            serde_json::from_str(&existing_raw)
                .context("failed to parse existing remote GitHub mode state")?,
        )
    };
    let record =
        merge_github_yolo_mode_records(existing_record, next_record, current_epoch_seconds()?);
    let raw =
        serde_json::to_string_pretty(&record).context("failed to encode GitHub mode state")?;
    let mut mode_file = NamedTempFile::new().context("failed to create GitHub mode temp file")?;
    use std::io::Write as _;
    mode_file
        .write_all(raw.as_bytes())
        .context("failed to write GitHub mode temp file")?;

    let repair_agent_git = github_yolo_agent_git_repair_script();
    let exec_args = vec![
        "bash".to_string(),
        "-lc".to_string(),
        format!(
            "sudo install -d -m 0750 -o zodex-publisher -g zodex {dir} && sudo install -m 0640 -o zodex-publisher -g zodex {tmp} {dest} && rm -f {tmp}\n{repair_agent_git}",
            dir = GITHUB_MODE_DIR,
            tmp = GITHUB_MODE_REMOTE_TMP_PATH,
            dest = GITHUB_MODE_STATE_PATH
        ),
    ];
    run_sprite_exec(
        &resolved.name,
        resolved.org.as_deref(),
        &exec_args,
        &[(mode_file.path(), GITHUB_MODE_REMOTE_TMP_PATH)],
    )?;

    let agent_git_status = inspect_github_yolo_agent_git_status(resolved)?;

    println!("github-mode: yolo");
    println!("sprite: {}", resolved.name);
    if let Some(org) = resolved.org.as_deref() {
        println!("org: {org}");
    }
    print_github_yolo_scope(&record, current_epoch_seconds()?);
    println!(
        "ttl: {}",
        if active_ttl.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("token-exposure: none");
    print_github_yolo_agent_git_status(&agent_git_status);
    println!(
        "direct-git-push: {}",
        if agent_git_status.direct_push_ready() {
            "enabled-via-zodex-remote-helper"
        } else {
            "broken-missing-agent-git-config"
        }
    );
    println!("direct-git-push-refs: refs/heads/* refs/tags/*");
    println!("push-grants: covered-by-yolo-mode");
    Ok(())
}

fn disable_github_yolo_mode(resolved: &ResolvedSprite) -> Result<()> {
    let exec_args = vec![
        "bash".to_string(),
        "-lc".to_string(),
        format!("sudo rm -f {GITHUB_MODE_STATE_PATH}"),
    ];
    run_sprite_exec(&resolved.name, resolved.org.as_deref(), &exec_args, &[])?;
    println!("github-mode: default");
    println!("sprite: {}", resolved.name);
    if let Some(org) = resolved.org.as_deref() {
        println!("org: {org}");
    }
    println!("yolo-state: removed");
    println!("push-grants: unchanged");
    Ok(())
}

fn print_github_mode_status(resolved: &ResolvedSprite) -> Result<()> {
    let exec_args = vec![
        "bash".to_string(),
        "-lc".to_string(),
        format!(
            "if sudo test -f {GITHUB_MODE_STATE_PATH}; then sudo cat {GITHUB_MODE_STATE_PATH}; fi"
        ),
    ];
    let raw = run_sprite_exec(&resolved.name, resolved.org.as_deref(), &exec_args, &[])?;
    println!("sprite: {}", resolved.name);
    if let Some(org) = resolved.org.as_deref() {
        println!("org: {org}");
    }
    if raw.trim().is_empty() {
        println!("github-mode: default");
        println!("direct-git-push: disabled");
        println!("push-grants: separate");
        return Ok(());
    }

    let record: GithubModeRecord =
        serde_json::from_str(&raw).context("failed to parse remote GitHub mode state")?;
    if record.mode != "yolo" {
        println!("github-mode: default");
        println!("mode-state: {}", record.mode);
        println!("direct-git-push: disabled");
        println!("push-grants: separate");
        return Ok(());
    }
    let now_epoch_seconds = current_epoch_seconds()?;
    if github_mode_expired(&record, now_epoch_seconds) {
        println!("github-mode: default");
        println!("yolo-state: expired");
        if let Some(expires_at) = record.expires_at.as_deref() {
            println!("expired-at: {expires_at}");
        }
        print_epoch_local_line("expired-at-local", record.expires_at_epoch_seconds);
        println!("direct-git-push: disabled");
        println!("push-grants: separate");
        return Ok(());
    }

    let agent_git_status = inspect_github_yolo_agent_git_status(resolved)?;

    println!("github-mode: yolo");
    print_github_yolo_scope(&record, now_epoch_seconds);
    println!("token-exposure: none");
    print_github_yolo_agent_git_status(&agent_git_status);
    println!(
        "direct-git-push: {}",
        if agent_git_status.direct_push_ready() {
            "enabled-via-zodex-remote-helper"
        } else {
            "broken-missing-agent-git-config"
        }
    );
    println!("direct-git-push-refs: refs/heads/* refs/tags/*");
    println!("push-grants: covered-by-yolo-mode");
    Ok(())
}
