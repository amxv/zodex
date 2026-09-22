    use super::{
        Commands, GithubYoloAgentGitStatus, MAX_GITHUB_ERROR_DETAIL_CHARS,
        OPERATOR_SPRITES_REGISTRY_VERSION,
        OperatorSpriteRecord, OperatorSpriteRegistry, PUBLISHER_SERVICE_LABEL, ProxyCommand,
        ProxyOriginResolution, ProxyWorkerStatus, PushGrantRecord,
        ResolvedSprite, SPRITE_MAIN_SERVICE_LABEL, SpriteCommand, SpriteGithubCommand,
        SpriteServiceAction, SpriteServiceState, SpriteServiceStatus, browser_open_attempts,
        build_github_yolo_agent_git_status_lines, build_github_yolo_mode_record,
        build_github_yolo_mode_record_at, build_sprite_api_args,
        build_sprite_detached_stop_script, build_sprite_services_status_lines, build_sprite_setup_script,
        build_sprite_upgrade_script,
        ensure_proxy_origin_is_publicly_routable, ensure_temporary_wrangler_version,
        expected_sprite_service_definitions, expected_zodex_agent_git_helper,
        github_mode_expired,
        github_default_agent_git_repair_script,
        github_yolo_agent_git_inspect_script, github_yolo_agent_git_repair_script,
        derive_proxy_worker_name,
        materialize_proxy_project,
        merge_github_yolo_mode_records, normalize_github_repo, normalize_github_repos,
        normalize_proxy_origin, parse_sprite_info, parse_wrangler_deploy_output, parse_wrangler_version,
        parse_github_yolo_agent_git_status, parse_push_grant_ttl, parse_push_grants,
        proxy_mcp_status_looks_healthy, proxy_worker_build_id, proxy_worker_build_state,
        push_grant_expired, render_proxy_wrangler_config, restart_sprite_service_stack_with,
        resolve_publisher_client_id, resolve_remote_sprite_from_registry,
        shell_escape_single_quotes, sprite_config_url_auth_args, sprite_info_args,
        sprite_service_logs_api_path, sprite_service_delete_order, strip_sprite_api_prelude,
        summarize_github_error_body,
        upsert_operator_sprite_record, validate_installed_sprite_release,
        validate_sprite_service_operation_stream,
    };
    #[cfg(unix)]
    use super::{ProxyDeployCommandSpec, execute_wrangler_deploy};
    #[cfg(not(target_os = "windows"))]
    use super::operator_sprites_registry_path_from_home;
    use crate::operator_cli::Cli;
    use clap::Parser;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    #[cfg(not(target_os = "windows"))]
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::tempdir;
    #[test]
    fn safe_sprite_operations_accept_omitted_sprite_at_parse_time() {
        for args in [
            vec!["zodex", "sprite", "upgrade"],
            vec!["zodex", "sprite", "sync"],
            vec!["zodex", "sprite", "status"],
            vec!["zodex", "sprite", "logs", "--service", "zodexd"],
            vec!["zodex", "sprite", "health"],
            vec!["zodex", "sprite", "restart"],
            vec!["zodex", "sprite", "connect"],
            vec!["zodex", "sprite", "proxy", "inspect"],
            vec!["zodex", "sprite", "github", "status"],
        ] {
            Cli::try_parse_from(args).expect("safe sprite operation should parse without --sprite");
        }
    }

    #[test]
    fn canonical_sprite_namespaces_parse_to_shared_command_types() {
        let proxy = Cli::try_parse_from([
            "zodex",
            "sprite",
            "proxy",
            "deploy",
            "--sprite",
            "dev",
            "--skip-verify-origin",
        ])
        .expect("nested Sprite proxy syntax should parse");
        assert!(matches!(
            proxy.command,
            Commands::Sprite {
                command: SpriteCommand::Proxy {
                    command: ProxyCommand::Deploy {
                        sprite: Some(ref sprite),
                        skip_verify_origin: true,
                        ..
                    }
                }
            } if sprite == "dev"
        ));

        let connect = Cli::try_parse_from([
            "zodex",
            "sprite",
            "connect",
            "--sprite",
            "dev",
            "--show-url",
        ])
        .expect("Sprite connect syntax should parse");
        assert!(matches!(
            connect.command,
            Commands::Sprite {
                command: SpriteCommand::Connect {
                    sprite: Some(ref sprite),
                    show_url: true,
                    ..
                }
            } if sprite == "dev"
        ));

        let github = Cli::try_parse_from([
            "zodex",
            "sprite",
            "github",
            "yolo",
            "--sprite",
            "dev",
            "--repo",
            "amxv/zodex",
            "--ttl",
            "45m",
        ])
        .expect("flattened nested Sprite GitHub syntax should parse");
        assert!(matches!(
            github.command,
            Commands::Sprite {
                command: SpriteCommand::Github {
                    command: SpriteGithubCommand::Yolo {
                        sprite: Some(ref sprite),
                        ref repos,
                        ref ttl,
                        no_ttl: false,
                        ..
                    }
                }
            } if sprite == "dev" && repos == &["amxv/zodex"] && ttl == "45m"
        ));
    }

    #[test]
    fn sprite_setup_keeps_sprite_required_at_parse_time() {
        let err = Cli::try_parse_from([
            "zodex",
            "sprite",
            "setup",
            "--repo",
            "amxv/zodex",
            "--reader-app-id",
            "1",
            "--reader-pem",
            "/tmp/reader.pem",
            "--publisher-app-id",
            "2",
            "--publisher-pem",
            "/tmp/publisher.pem",
        ])
        .expect_err("setup should still require --sprite");

        assert!(err.to_string().contains("--sprite"));
    }

    #[test]
    fn shell_escape_single_quotes_handles_embedded_quotes() {
        assert_eq!(shell_escape_single_quotes("v0.1.5's"), "'v0.1.5'\"'\"'s'");
    }

    #[test]
    fn expected_sprite_service_definitions_use_config_path() {
        let defs = expected_sprite_service_definitions(Path::new("/etc/zodex/custom.toml"));

        assert_eq!(
            defs.get(PUBLISHER_SERVICE_LABEL)
                .expect("publisher definition")
                .args,
            vec![
                "-n".to_string(),
                "-u".to_string(),
                "zodex-publisher".to_string(),
                "/usr/local/bin/zodex-prd".to_string(),
                "--config".to_string(),
                "/etc/zodex/custom.toml".to_string(),
            ]
        );
        assert_eq!(
            defs.get(SPRITE_MAIN_SERVICE_LABEL)
                .expect("main definition")
                .http_port,
            Some(8080)
        );
    }

    #[test]
    fn sprite_detached_stop_uses_exact_commands_in_dependency_order() {
        let script = build_sprite_detached_stop_script(Path::new("/etc/zodex/custom.toml"));
        let daemon = "pkill -f -x -- '/usr/local/bin/zodexd --config /etc/zodex/custom.toml'";
        let publisher =
            "pkill -f -x -- '/usr/local/bin/zodex-prd --config /etc/zodex/custom.toml'";

        assert!(script.contains(daemon));
        assert!(script.contains(publisher));
        assert!(script.find(daemon) < script.find(publisher));
        assert!(!script.contains("pkill -f --"));
    }

    #[test]
    fn sprite_force_recreate_deletes_dependent_before_dependency() {
        assert_eq!(
            sprite_service_delete_order(),
            [SPRITE_MAIN_SERVICE_LABEL, PUBLISHER_SERVICE_LABEL]
        );
    }

    #[test]
    fn sprite_service_operation_stream_requires_terminal_complete_and_rejects_error() {
        validate_sprite_service_operation_stream(
            SPRITE_MAIN_SERVICE_LABEL,
            SpriteServiceAction::Start,
            r#"{"type":"started"}
{"type":"stdout","data":"ready\n"}
{"type":"complete"}
"#,
        )
        .expect("complete NDJSON stream should pass");

        let error = validate_sprite_service_operation_stream(
            PUBLISHER_SERVICE_LABEL,
            SpriteServiceAction::Restart,
            r#"{"type":"stopping"}
{"type":"error","data":"publisher failed"}
{"type":"complete"}
"#,
        )
        .expect_err("streamed error must fail even if complete follows");
        assert!(error.to_string().contains("publisher failed"));

        let incomplete = validate_sprite_service_operation_stream(
            SPRITE_MAIN_SERVICE_LABEL,
            SpriteServiceAction::Stop,
            r#"{"type":"stopping"}
{"type":"stopped"}
"#,
        )
        .expect_err("missing terminal complete must fail");
        assert!(incomplete.to_string().contains("terminal `complete`"));

        validate_sprite_service_operation_stream(
            SPRITE_MAIN_SERVICE_LABEL,
            SpriteServiceAction::Stop,
            "service is not running\n",
        )
        .expect("stopping an already-stopped service should be idempotent");

        let start_plaintext = validate_sprite_service_operation_stream(
            SPRITE_MAIN_SERVICE_LABEL,
            SpriteServiceAction::Start,
            "service is not running\n",
        )
        .expect_err("plain-text not-running response is only valid for stop");
        assert!(start_plaintext
            .to_string()
            .contains("failed to parse Sprite Service NDJSON event"));
    }

    #[test]
    fn sprite_restart_stops_dependent_restarts_publisher_then_starts_and_verifies_daemon() {
        let publisher_running = r#"[
            {"name":"zodex-prd","cmd":"sudo","args":[],"needs":[],"http_port":null,"state":{"status":"running"}},
            {"name":"zodexd","cmd":"sudo","args":[],"needs":["zodex-prd"],"http_port":8080,"state":{"status":"stopped"}}
        ]"#;
        let all_running = r#"[
            {"name":"zodex-prd","cmd":"sudo","args":[],"needs":[],"http_port":null,"state":{"status":"running"}},
            {"name":"zodexd","cmd":"sudo","args":[],"needs":["zodex-prd"],"http_port":8080,"state":{"status":"running"}}
        ]"#;
        let mut calls = Vec::new();

        restart_sprite_service_stack_with(|path, args| {
            calls.push((path.to_string(), args.to_vec()));
            match calls.len() {
                1 => Ok("{\"type\":\"stopping\"}\n{\"type\":\"stopped\"}\n{\"type\":\"complete\"}\n".to_string()),
                2 => Ok("{\"type\":\"stopping\"}\n{\"type\":\"started\"}\n{\"type\":\"complete\"}\n".to_string()),
                3 => Ok(publisher_running.to_string()),
                4 => Ok("{\"type\":\"started\"}\n{\"type\":\"complete\"}\n".to_string()),
                5 => Ok(all_running.to_string()),
                _ => panic!("unexpected Sprite API call {path}"),
            }
        })
        .expect("dependency-safe restart should succeed");

        assert_eq!(
            calls.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(),
            vec![
                "/services/zodexd/stop",
                "/services/zodex-prd/restart",
                "/services",
                "/services/zodexd/start",
                "/services",
            ]
        );
        assert_eq!(calls[0].1, vec!["-sS", "-X", "POST"]);
        assert_eq!(calls[1].1, vec!["-sS", "-X", "POST"]);
        assert_eq!(calls[2].1, vec!["-sS"]);
        assert_eq!(calls[3].1, vec!["-sS", "-X", "POST"]);
        assert_eq!(calls[4].1, vec!["-sS"]);
    }

    #[test]
    fn sprite_restart_rejects_non_running_final_service_state() {
        let publisher_running = r#"[
            {"name":"zodex-prd","cmd":"sudo","args":[],"needs":[],"http_port":null,"state":{"status":"running"}},
            {"name":"zodexd","cmd":"sudo","args":[],"needs":["zodex-prd"],"http_port":8080,"state":{"status":"stopped"}}
        ]"#;
        let daemon_failed = r#"[
            {"name":"zodex-prd","cmd":"sudo","args":[],"needs":[],"http_port":null,"state":{"status":"running"}},
            {"name":"zodexd","cmd":"sudo","args":[],"needs":["zodex-prd"],"http_port":8080,"state":{"status":"failed"}}
        ]"#;
        let mut call = 0usize;
        let error = restart_sprite_service_stack_with(|_, _| {
            call += 1;
            match call {
                1 => Ok("{\"type\":\"stopped\"}\n{\"type\":\"complete\"}\n".to_string()),
                2 => Ok("{\"type\":\"started\"}\n{\"type\":\"complete\"}\n".to_string()),
                3 => Ok(publisher_running.to_string()),
                4 => Ok("{\"type\":\"started\"}\n{\"type\":\"complete\"}\n".to_string()),
                5 => Ok(daemon_failed.to_string()),
                _ => unreachable!(),
            }
        })
        .expect_err("final failed service must reject restart");

        assert!(error.to_string().contains("zodexd is failed"));
    }

    #[test]
    fn installed_sprite_release_validation_accepts_tags_and_rejects_drift() {
        assert_eq!(
            validate_installed_sprite_release("zodex-agent 0.3.1\n", "v0.3.1")
                .expect("matching tag should pass"),
            "0.3.1"
        );
        assert!(
            validate_installed_sprite_release("zodex-agent 0.3.0\n", "v0.3.1")
                .expect_err("version drift should fail")
                .to_string()
                .contains("expected 0.3.1")
        );
    }

    #[test]
    fn build_sprite_api_args_include_scope_and_passthrough_curl_flags() {
        let args = build_sprite_api_args(
            "spritebox",
            Some("amxv"),
            "/services",
            &["-sS".to_string(), "-X".to_string(), "PUT".to_string()],
        );

        assert_eq!(
            args,
            vec![
                "api".to_string(),
                "-o".to_string(),
                "amxv".to_string(),
                "-s".to_string(),
                "spritebox".to_string(),
                "/services".to_string(),
                "--".to_string(),
                "-sS".to_string(),
                "-X".to_string(),
                "PUT".to_string(),
            ]
        );
    }

    #[test]
    fn strip_sprite_api_prelude_removes_wrapper_lines() {
        let raw = "Calling API: amxv spritebox\nURL: https://api.sprites.dev/v1/sprites/spritebox/services\n\n[]\n";
        assert_eq!(strip_sprite_api_prelude(raw), "[]\n");
    }

    #[test]
    fn sprite_service_logs_api_path_adds_optional_query_params() {
        assert_eq!(
            sprite_service_logs_api_path("zodexd", Some(50), Some("5s")),
            "/services/zodexd/logs?lines=50&duration=5s"
        );
        assert_eq!(
            sprite_service_logs_api_path("zodexd", None, None),
            "/services/zodexd/logs"
        );
    }

    #[test]
    fn build_sprite_services_status_lines_report_missing_services() {
        let lines = build_sprite_services_status_lines(
            Path::new("/etc/zodex/config.toml"),
            "spritebox",
            &[],
        );
        let joined = lines.join("\n");

        assert!(joined.contains("service-mode: sprite-services"));
        assert!(joined.contains("service: zodex-prd"));
        assert!(joined.contains("active: missing"));
        assert!(joined.contains("service: zodexd"));
        assert!(joined.contains(
            "hint: register Sprite Services with `zodex sprite sync --sprite spritebox`"
        ));
    }

    #[test]
    fn build_sprite_services_status_lines_report_definition_drift() {
        let services = vec![
            SpriteServiceStatus {
                name: PUBLISHER_SERVICE_LABEL.to_string(),
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "zodex-publisher".to_string(),
                    "/usr/local/bin/zodex-prd".to_string(),
                    "--config".to_string(),
                    "/etc/zodex/config.toml".to_string(),
                ],
                needs: Vec::new(),
                http_port: None,
                state: Some(SpriteServiceState {
                    name: Some(PUBLISHER_SERVICE_LABEL.to_string()),
                    pid: Some(111),
                    started_at: Some("2026-03-21T08:00:00Z".to_string()),
                    status: Some("running".to_string()),
                }),
            },
            SpriteServiceStatus {
                name: SPRITE_MAIN_SERVICE_LABEL.to_string(),
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "zodex-agent".to_string(),
                    "/usr/local/bin/zodexd".to_string(),
                    "--config".to_string(),
                    "/etc/zodex/other.toml".to_string(),
                ],
                needs: vec![PUBLISHER_SERVICE_LABEL.to_string()],
                http_port: Some(8080),
                state: Some(SpriteServiceState {
                    name: Some(SPRITE_MAIN_SERVICE_LABEL.to_string()),
                    pid: Some(222),
                    started_at: Some("2026-03-21T08:01:00Z".to_string()),
                    status: Some("starting".to_string()),
                }),
            },
        ];

        let lines = build_sprite_services_status_lines(
            Path::new("/etc/zodex/config.toml"),
            "spritebox",
            &services,
        );
        let joined = lines.join("\n");

        assert!(joined.contains("service: zodexd"));
        assert!(joined.contains("active: starting"));
        assert!(joined.contains("definition-match: no"));
        assert!(joined.contains("hint: re-sync with `zodex sprite sync --sprite spritebox`"));
        assert!(joined.contains(
            "hint: inspect logs with `zodex sprite logs --sprite spritebox --service zodexd`"
        ));
    }
