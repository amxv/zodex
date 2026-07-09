pub(crate) async fn run() -> Result<()> {
    install_rustls_crypto_provider();

    let cli = Cli::parse();
    let config_path = PathBuf::from(&cli.config);

    match cli.command {
        Commands::Install => {
            install(&config_path)?;
        }
        Commands::Upgrade { version } => {
            upgrade(&config_path, &version)?;
        }
        Commands::Start => {
            ensure_linux()?;
            start_stack(&config_path)?;
        }
        Commands::Stop => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            stop_stack(&config)?;
        }
        Commands::Restart => {
            ensure_linux()?;
            restart_stack(&config_path)?;
        }
        Commands::Status => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            print_stack_status_summary(&config)?;
        }
        Commands::Logs => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            match detect_service_manager() {
                ServiceManager::Systemd => {
                    let logs = run_journalctl(&build_journalctl_args())?;
                    if logs.is_empty() {
                        println!("no recent logs found for {SERVICE_NAME}");
                    } else {
                        print!("{}", redact_api_key_query_params(&logs));
                    }
                }
                ServiceManager::Process => {
                    let logs =
                        read_process_logs(&config, DEFAULT_LOG_LINES.parse().unwrap_or(200))?;
                    if logs.is_empty() {
                        println!(
                            "no recent logs found for {}",
                            process_log_path(&config).display()
                        );
                    } else {
                        print!("{}", redact_api_key_query_params(&logs));
                    }
                }
            }
        }
        Commands::SetKey { value } => {
            let mut config = Config::load(Some(Path::new(&config_path)))?;
            config.api_key = value;
            config.save(&config_path)?;
            ensure_shared_group_permissions(&config, &config_path)?;
            println!("updated API key in {}", config_path.display());
        }
        Commands::RotateKey => {
            let mut config = Config::load(Some(Path::new(&config_path)))?;
            let mut rng = rand::rng();
            config.api_key = Alphanumeric.sample_string(&mut rng, 48);
            config.save(&config_path)?;
            ensure_shared_group_permissions(&config, &config_path)?;
            println!("rotated API key in {}", config_path.display());
        }
        Commands::GitCredentialHelper { operation } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            handle_git_credential_helper(&config, &operation).await?;
        }
        Commands::ShowUrl { host } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            let raw_url = format!("https://{host}/mcp?key={}", config.api_key);
            println!(
                "{} (key redacted in CLI output)",
                redact_api_key_query_params(&raw_url)
            );
        }
        Commands::Tls { command } => match command {
            TlsCommand::Setup => tls_setup(&config_path)?,
        },
        Commands::Publisher { command } => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            match command {
                PublisherCommand::Start => start_publisher_process_mode(&config, &config_path)?,
                PublisherCommand::Stop => stop_publisher_process_mode(&config)?,
                PublisherCommand::Status => print_publisher_status_summary(&config),
                PublisherCommand::Logs => {
                    let logs =
                        read_publisher_logs(&config, DEFAULT_LOG_LINES.parse().unwrap_or(200))?;
                    if logs.is_empty() {
                        println!(
                            "no recent logs found for {}",
                            publisher_process_log_path(&config).display()
                        );
                    } else {
                        print!("{logs}");
                    }
                }
            }
        }
        Commands::Sprite { command } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            match command {
                SpriteCommand::Setup {
                    sprite,
                    org,
                    repo,
                    reader_app_id,
                    reader_pem,
                    publisher_app_id,
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
                    )?;
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
                        &config,
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
                    verify_sprite_health(
                        &resolved.name,
                        resolved.org.as_deref(),
                        url_auth.as_deref(),
                    )?;
                }
            }
        }
        Commands::Proxy { command } => match command {
            ProxyCommand::Inspect {
                sprite,
                org,
                origin,
            } => {
                inspect_proxy_component(sprite.as_deref(), org.as_deref(), origin.as_deref())?;
            }
            ProxyCommand::Deploy {
                sprite,
                org,
                origin,
                skip_verify_origin,
            } => {
                deploy_proxy_component(
                    sprite.as_deref(),
                    org.as_deref(),
                    origin.as_deref(),
                    skip_verify_origin,
                )?;
            }
            ProxyCommand::VerifyOrigin {
                sprite,
                org,
                origin,
            } => {
                verify_proxy_origin_command(sprite.as_deref(), org.as_deref(), origin.as_deref())?;
            }
        },
        Commands::Github { command } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            match command {
                GithubCommand::RequestPush {
                    repo,
                    publisher_client_id,
                    ttl,
                    no_ttl,
                    cache_refresh_token,
                } => {
                    let ttl = if no_ttl {
                        None
                    } else if ttl == "30m" {
                        Some(Duration::from_secs(DEFAULT_PUSH_GRANT_TTL_SECONDS))
                    } else {
                        Some(parse_push_grant_ttl(&ttl)?)
                    };
                    request_push_access(
                        &config,
                        &repo,
                        publisher_client_id.as_deref(),
                        ttl,
                        cache_refresh_token,
                    )
                    .await?;
                }
                GithubCommand::GrantPush {
                    sprite,
                    repo,
                    org,
                    publisher_client_id,
                } => {
                    let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                    grant_push_access(
                        &config,
                        &resolved.name,
                        resolved.org.as_deref(),
                        &repo,
                        publisher_client_id.as_deref(),
                    )
                    .await?;
                }
                GithubCommand::RevokePush {
                    sprite,
                    repo,
                    org,
                    forget_local_auth,
                } => {
                    revoke_push_access(
                        sprite.as_deref(),
                        org.as_deref(),
                        &repo,
                        forget_local_auth,
                    )?;
                }
                GithubCommand::ListGrants { sprite, org } => {
                    list_push_grants(sprite.as_deref(), org.as_deref())?;
                }
                GithubCommand::Mode { command } => match command {
                    GithubModeCommand::Yolo {
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
                        enable_github_yolo_mode(&resolved, &repos, ttl)?;
                    }
                    GithubModeCommand::Default { sprite, org } => {
                        let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                        disable_github_yolo_mode(&resolved)?;
                    }
                    GithubModeCommand::Status { sprite, org } => {
                        let resolved = resolve_remote_sprite(sprite.as_deref(), org.as_deref())?;
                        print_github_mode_status(&resolved)?;
                    }
                },
            }
        }
    }

    Ok(())
}
