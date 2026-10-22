pub(crate) async fn run() -> Result<()> {
    install_rustls_crypto_provider();

    let cli = Cli::parse();

    match cli.command {
        Commands::Upgrade {
            version,
            check,
            format,
            stop_local,
            refresh,
        } => {
            upgrade_operator(OperatorUpgradeOptions {
                version: &version,
                check,
                format,
                stop_local,
                refresh,
                automatic_restart: false,
            })
            .await?;
        }
        Commands::Sprite { command } => {
            match command {
                SpriteCommand::Setup {
                    sprite,
                    org,
                    repo,
                    reader_app_id,
                    reader_pem,
                    publisher_app_id,
                    publisher_client_id,
                    publisher_pem,
                    default_base,
                    url_auth,
                    remote_config,
                } => {
                    sprite_setup(SpriteSetupOptions {
                        sprite: &sprite,
                        org: org.as_deref(),
                        repo: &repo,
                        reader_app_id,
                        reader_pem: &reader_pem,
                        publisher_app_id,
                        publisher_client_id: &publisher_client_id,
                        publisher_pem: &publisher_pem,
                        default_base: &default_base,
                        url_auth: &url_auth,
                        remote_config: Path::new(&remote_config),
                    })
                    .await?;
                }
                SpriteCommand::Upgrade {
                    sprite,
                    org,
                    version,
                    repo,
                    url_auth,
                    remote_config,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    sprite_upgrade(
                        &resolved.name,
                        resolved.org.as_deref(),
                        &version,
                        repo.as_deref(),
                        url_auth.as_deref(),
                        Path::new(&remote_config),
                    )
                    .await?;
                }
                SpriteCommand::Sync {
                    sprite,
                    org,
                    remote_config,
                    force_recreate,
                    skip_stop_detached,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    sync_sprite_services(
                        &resolved.name,
                        resolved.org.as_deref(),
                        Path::new(&remote_config),
                        force_recreate,
                        skip_stop_detached,
                    )?;
                }
                SpriteCommand::Status {
                    sprite,
                    org,
                    remote_config,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    print_sprite_services_status_summary(
                        Path::new(&remote_config),
                        &resolved.name,
                        resolved.org.as_deref(),
                    )?;
                }
                SpriteCommand::Logs {
                    sprite,
                    service,
                    org,
                    lines,
                    duration,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    print_sprite_service_logs(
                        &resolved.name,
                        resolved.org.as_deref(),
                        &service,
                        lines,
                        duration.as_deref(),
                    )?;
                }
                SpriteCommand::Health {
                    sprite,
                    org,
                    url_auth,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    if let Some(url_auth) = url_auth.as_deref() {
                        require_public_sprite_url_auth(url_auth)?;
                    }
                    let record = load_operator_sprite_record(&resolved)?.ok_or_else(|| {
                        anyhow!(
                            "Sprite `{}` is not registered locally; run `zodex sprite setup` first",
                            resolved.name
                        )
                    })?;
                    verify_sprite_end_to_end_health(&resolved, &record).await?;
                }
                SpriteCommand::Restart { sprite, org } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    restart_sprite_services(&resolved.name, resolved.org.as_deref())?;
                }
                SpriteCommand::Connect {
                    sprite,
                    org,
                    show_url,
                } => {
                    connect_sprite(sprite.as_deref(), org.as_deref(), show_url)?;
                }
                SpriteCommand::Proxy { command } => {
                    handle_proxy_command(command)?;
                }
                SpriteCommand::Github { command } => {
                    handle_sprite_github_command(command).await?;
                }
            }
        }
        Commands::Local { command } => {
            handle_local_command(command).await?;
        }
    }

    Ok(())
}

fn handle_proxy_command(command: ProxyCommand) -> Result<()> {
    match command {
        ProxyCommand::Status {
            sprite,
            org,
            origin,
            worker_name,
            worker_url,
        } => inspect_proxy_component(
            sprite.as_deref(),
            org.as_deref(),
            origin.as_deref(),
            worker_name.as_deref(),
            worker_url.as_deref(),
        ),
        ProxyCommand::Deploy {
            sprite,
            org,
            origin,
            worker_name,
            cloudflare_account,
            skip_verify_origin,
        } => deploy_proxy_component(
            sprite.as_deref(),
            org.as_deref(),
            origin.as_deref(),
            worker_name.as_deref(),
            cloudflare_account.as_deref(),
            skip_verify_origin,
        ),
        ProxyCommand::Verify {
            sprite,
            org,
            origin,
            worker_url,
        } => verify_proxy_command(
            sprite.as_deref(),
            org.as_deref(),
            origin.as_deref(),
            worker_url.as_deref(),
        ),
    }
}

async fn handle_sprite_github_command(command: SpriteGithubCommand) -> Result<()> {
    match command {
        SpriteGithubCommand::GrantPush {
            sprite,
            repo,
            org,
            publisher_client_id,
        } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            grant_push_access(
                &resolved.name,
                resolved.org.as_deref(),
                &repo,
                publisher_client_id.as_deref(),
            )
            .await
        }
        SpriteGithubCommand::RevokePush {
            sprite,
            repo,
            org,
            forget_local_auth,
        } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            revoke_push_access(
                &resolved.name,
                resolved.org.as_deref(),
                &repo,
                forget_local_auth,
            )
        }
        SpriteGithubCommand::ListGrants { sprite, org } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            list_push_grants(&resolved.name, resolved.org.as_deref())
        }
        SpriteGithubCommand::Yolo {
            sprite,
            org,
            repos,
            ttl,
            no_ttl,
        } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            let ttl = if no_ttl {
                None
            } else if ttl == "2h" {
                Some(Duration::from_secs(DEFAULT_YOLO_TTL_SECONDS))
            } else {
                Some(parse_push_grant_ttl(&ttl)?)
            };
            enable_github_yolo_mode(&resolved, &repos, ttl)
        }
        SpriteGithubCommand::Default { sprite, org } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            disable_github_yolo_mode(&resolved)
        }
        SpriteGithubCommand::Status { sprite, org } => {
            let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
            print_github_mode_status(&resolved)
        }
    }
}
