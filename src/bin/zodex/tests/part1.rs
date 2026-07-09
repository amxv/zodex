    use super::{
        DEFAULT_LOG_LINES, GithubYoloAgentGitStatus, OperatorSpriteRecord, OperatorSpriteRegistry,
        PUBLISHER_SERVICE_LABEL, ProcessModeState, PushGrantRecord, SERVICE_NAME,
        SPRITE_MAIN_SERVICE_LABEL, ServiceManager, SpriteServiceState, SpriteServiceStatus,
        SystemctlAction, browser_open_attempts, build_certbot_args,
        build_github_yolo_agent_git_status_lines, build_github_yolo_mode_record,
        build_github_yolo_mode_record_at, build_journalctl_args, build_operator_upgrade_shell_args,
        build_process_status_lines, build_publisher_status_lines, build_reader_status_lines,
        build_runtime_upgrade_shell_args, build_sprite_api_args,
        build_sprite_services_status_lines, build_sprite_setup_script, build_sprite_upgrade_script,
        build_status_summary_lines, build_systemctl_args, certbot_cert_name,
        credential_host_is_github, credential_url_host, credential_url_path,
        credential_url_protocol, ensure_http_listener_ready_for_start,
        expected_sprite_service_definitions, expected_zodex_agent_git_helper,
        generate_self_signed_certificate, git_credential_request_repo,
        git_credential_request_targets_github, github_mode_expired,
        github_yolo_agent_git_inspect_script, github_yolo_agent_git_repair_script,
        load_matching_push_grant, load_push_grant_from_dir, merge_github_yolo_mode_records,
        normalize_github_repo, normalize_github_repos, normalize_proxy_origin,
        operator_sprites_registry_path_from_home, parse_git_credential_request,
        parse_github_yolo_agent_git_status, parse_push_grant_ttl, parse_push_grants,
        parse_systemctl_show, process_log_path, process_pid_path, proxy_mcp_status_looks_healthy,
        push_grant_expired, read_tail_lines, render_proxy_wrangler_config, render_systemd_unit,
        resolve_publisher_client_id, resolve_remote_sprite_from_registry, select_tls_san_ip,
        service_manager_from_pid1, shell_escape_single_quotes, sprite_service_logs_api_path,
        sprite_service_supervisor_pids_from_ps, state_root_for_config, status_host_hint,
        strip_sprite_api_prelude, tls_artifacts_exist, upsert_operator_sprite_record,
        write_if_changed,
    };
    use crate::operator_cli::Cli;
    use clap::{CommandFactory, Parser};
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::tempdir;
    use zodex::config::Config;

    #[test]
    fn clap_help_uses_zodex_name() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("zodex"));
        assert!(help.contains("Zodex operator CLI"));
        assert!(!help.contains("publish-pr"));
        assert!(!help.contains("\npublisher"));
    }

    #[test]
    fn safe_sprite_operations_accept_omitted_sprite_at_parse_time() {
        for args in [
            vec!["zodex", "sprite", "upgrade"],
            vec!["zodex", "sprite", "sync"],
            vec!["zodex", "sprite", "status"],
            vec!["zodex", "sprite", "logs", "--service", "zodexd"],
            vec!["zodex", "sprite", "health"],
        ] {
            Cli::try_parse_from(args).expect("safe sprite operation should parse without --sprite");
        }
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
    fn render_systemd_unit_contains_expected_execstart() {
        let unit = render_systemd_unit(
            Path::new("/usr/local/bin/zodexd"),
            Path::new("/etc/zodex/config.toml"),
        );
        assert!(unit.contains("[Service]"));
        assert!(
            unit.contains(
                "ExecStart=\"/usr/local/bin/zodexd\" --config \"/etc/zodex/config.toml\""
            )
        );
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn build_systemctl_args_match_expected_shapes() {
        assert_eq!(
            build_systemctl_args(SystemctlAction::DaemonReload),
            vec!["daemon-reload"]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Enable),
            vec!["enable", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Start),
            vec!["start", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Stop),
            vec!["stop", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Restart),
            vec!["restart", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::ShowStatus),
            vec![
                "show",
                SERVICE_NAME,
                "--property=ActiveState,SubState,UnitFileState,FragmentPath,ExecMainStatus",
                "--no-pager",
            ]
        );
    }

    #[test]
    fn build_journalctl_args_match_expected_shape() {
        assert_eq!(
            build_journalctl_args(),
            vec!["-u", SERVICE_NAME, "-n", DEFAULT_LOG_LINES, "--no-pager",]
        );
    }

    #[test]
    fn build_runtime_upgrade_shell_args_include_requested_version_and_http_port() {
        let config = Config {
            http_bind_port: Some(8080),
            ..Config::default()
        };

        let args = build_runtime_upgrade_shell_args("v0.1.5", &config);
        assert_eq!(args[0], "-lc");
        assert!(args[1].contains("export ZODEX_VERSION='v0.1.5'"));
        assert!(args[1].contains("export ZODEX_SOURCE_REF='v0.1.5'"));
        assert!(args[1].contains("export ZODEX_HTTP_BIND_PORT=8080"));
        assert!(args[1].contains("curl -fsSL 'https://zodex.ashray.xyz/install.sh' | bash"));
    }

    #[test]
    fn build_runtime_upgrade_shell_args_latest_uses_public_installer() {
        let config = Config::default();

        let args = build_runtime_upgrade_shell_args("latest", &config);
        assert!(args[1].contains("export ZODEX_VERSION='latest'"));
        assert!(!args[1].contains("ZODEX_SOURCE_REF"));
        assert!(args[1].contains("curl -fsSL 'https://zodex.ashray.xyz/install.sh' | bash"));
    }

    #[test]
    fn build_operator_upgrade_shell_args_uses_public_installer() {
        let args = build_operator_upgrade_shell_args("v0.2.0");
        assert_eq!(args[0], "-lc");
        assert!(args[1].contains("export ZODEX_INSTALL_MODE=operator"));
        assert!(args[1].contains("export ZODEX_VERSION='v0.2.0'"));
        assert!(args[1].contains("curl -fsSL 'https://zodex.ashray.xyz/install.sh' | sh"));
    }

    #[test]
    fn shell_escape_single_quotes_handles_embedded_quotes() {
        assert_eq!(shell_escape_single_quotes("v0.1.5's"), "'v0.1.5'\"'\"'s'");
    }

    #[test]
    fn normalize_proxy_origin_strips_trailing_slash() {
        let origin = normalize_proxy_origin("https://zodex.example.sprites.app/").expect("origin");
        assert_eq!(origin, "https://zodex.example.sprites.app");
    }

    #[test]
    fn normalize_proxy_origin_rejects_paths() {
        let err =
            normalize_proxy_origin("https://zodex.example.sprites.app/mcp").expect_err("path");
        assert!(err.to_string().contains("must not include a path"));
    }

    #[test]
    fn render_proxy_wrangler_config_replaces_origin_placeholder() {
        let rendered = render_proxy_wrangler_config(
            r#"{"vars":{"SPRITE_ORIGIN":"__SPRITE_ORIGIN__"}}"#,
            "https://zodex.example.sprites.app",
        )
        .expect("render");
        assert!(rendered.contains("https://zodex.example.sprites.app"));
        assert!(!rendered.contains("__SPRITE_ORIGIN__"));
    }

    #[test]
    fn proxy_mcp_status_looks_healthy_accepts_auth_or_success() {
        assert!(proxy_mcp_status_looks_healthy(200));
        assert!(proxy_mcp_status_looks_healthy(401));
        assert!(!proxy_mcp_status_looks_healthy(404));
    }

    #[test]
    fn parse_systemctl_show_extracts_values() {
        let raw = "ActiveState=active\nSubState=running\nUnitFileState=enabled\nFragmentPath=/etc/systemd/system/zodexd.service\nExecMainStatus=0\n";
        let parsed = parse_systemctl_show(raw);

        assert_eq!(
            parsed.get("ActiveState").map(String::as_str),
            Some("active")
        );
        assert_eq!(parsed.get("SubState").map(String::as_str), Some("running"));
        assert_eq!(
            parsed.get("UnitFileState").map(String::as_str),
            Some("enabled")
        );
        assert_eq!(
            parsed.get("FragmentPath").map(String::as_str),
            Some("/etc/systemd/system/zodexd.service")
        );
        assert_eq!(parsed.get("ExecMainStatus").map(String::as_str), Some("0"));
    }

    #[test]
    fn write_if_changed_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("zodexd.service");
        let content = "[Unit]\nDescription=test\n";

        let first = write_if_changed(&path, content).expect("first write");
        let second = write_if_changed(&path, content).expect("second write");

        assert!(first);
        assert!(!second);
        assert_eq!(fs::read_to_string(path).expect("read file"), content);
    }

    #[test]
    fn service_manager_from_pid1_detects_systemd() {
        assert_eq!(
            service_manager_from_pid1("systemd"),
            ServiceManager::Systemd
        );
        assert_eq!(
            service_manager_from_pid1("start.sh"),
            ServiceManager::Process
        );
    }

    #[test]
    fn state_root_for_config_uses_tls_parent_directory() {
        let config = Config {
            tls_cert_path: "/custom/state/tls/cert.pem".to_string(),
            ..Config::default()
        };

        assert_eq!(
            state_root_for_config(&config),
            PathBuf::from("/custom/state")
        );
        assert_eq!(
            process_pid_path(&config),
            PathBuf::from("/custom/state/run/zodexd.pid")
        );
        assert_eq!(
            process_log_path(&config),
            PathBuf::from("/custom/state/logs/zodexd.log")
        );
    }

    #[test]
    fn read_tail_lines_returns_only_requested_suffix() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("zodexd.log");
        fs::write(&path, "one\ntwo\nthree\nfour\n").expect("write log");

        let got = read_tail_lines(&path, 2).expect("read tail");
        assert_eq!(got, "three\nfour\n");
    }

    #[test]
    fn certbot_helpers_build_expected_values() {
        let ip: IpAddr = "203.0.113.42".parse().expect("ip parse");
        let cert_name = certbot_cert_name(ip);
        assert_eq!(cert_name, "zodex-203-0-113-42");

        let args = build_certbot_args(ip, &cert_name);
        assert!(args.contains(&"certonly".to_string()));
        assert!(args.contains(&"--standalone".to_string()));
        assert!(args.contains(&"--non-interactive".to_string()));
        assert!(args.contains(&"--cert-name".to_string()));
        assert!(args.contains(&cert_name));
        assert!(args.contains(&ip.to_string()));
    }

    #[test]
    fn select_tls_san_ip_prefers_public_ip() {
        let public = Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
        let selected = select_tls_san_ip("0.0.0.0", public);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn select_tls_san_ip_falls_back_to_bind_host() {
        let selected = select_tls_san_ip("192.0.2.10", None);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
    }

    #[test]
    fn select_tls_san_ip_defaults_to_loopback() {
        let selected = select_tls_san_ip("0.0.0.0", None);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn status_host_hint_uses_public_ip_for_wildcard_bind() {
        let hint = status_host_hint("0.0.0.0", Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11))));
        assert_eq!(hint, "203.0.113.11");
    }

    #[test]
    fn status_host_hint_uses_specific_bind_host() {
        let hint = status_host_hint("192.0.2.5", None);
        assert_eq!(hint, "192.0.2.5");
    }

    #[test]
    fn status_host_hint_returns_placeholder_without_public_ip() {
        let hint = status_host_hint("::", None);
        assert_eq!(hint, "<host>");
    }

    #[test]
    fn build_status_summary_lines_includes_network_and_tls_details() {
        let raw = "ActiveState=active\nSubState=running\nUnitFileState=enabled\nExecMainStatus=0\n";
        let config = Config {
            bind_host: "0.0.0.0".to_string(),
            bind_port: 8443,
            http_bind_port: Some(8080),
            api_key: "abc123".to_string(),
            tls_mode: "self_signed".to_string(),
            tls_cert_path: "/var/lib/zodex/tls/cert.pem".to_string(),
            tls_key_path: "/var/lib/zodex/tls/key.pem".to_string(),
            ..Config::default()
        };

        let lines = build_status_summary_lines(
            raw,
            &config,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 88))),
        );
        let joined = lines.join("\n");
        assert!(joined.contains("listen: 0.0.0.0:8443"));
        assert!(joined.contains("tls-mode: self_signed"));
        assert!(joined.contains("tls-cert: /var/lib/zodex/tls/cert.pem"));
        assert!(joined.contains("tls-key: /var/lib/zodex/tls/key.pem"));
        assert!(joined.contains("url-hint: https://198.51.100.88/mcp?key=<redacted>"));
        assert!(joined.contains("health-hint: https://198.51.100.88/health"));
        assert!(joined.contains("http-proxy-listen: 0.0.0.0:8080"));
    }

    #[test]
    fn build_process_status_lines_includes_process_mode_details() {
        let config = Config {
            bind_host: "0.0.0.0".to_string(),
            bind_port: 9443,
            http_bind_port: Some(8080),
            api_key: "abc123".to_string(),
            tls_mode: "self_signed".to_string(),
            tls_cert_path: "/var/lib/zodex/tls/cert.pem".to_string(),
            tls_key_path: "/var/lib/zodex/tls/key.pem".to_string(),
            ..Config::default()
        };

        let lines = build_process_status_lines(
            &config,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 88))),
            Ok(ProcessModeState::Running(4242)),
        )
        .expect("build process status");
        let joined = lines.join("\n");
        assert!(joined.contains("service-mode: process"));
        assert!(joined.contains("active: active (running)"));
        assert!(joined.contains("exec-main-status: running pid 4242"));
        assert!(joined.contains("agent-home: /home/zodex-agent"));
        assert!(joined.contains("default-workdir: /workspace"));
        assert!(joined.contains("url-hint: https://198.51.100.88/mcp?key=<redacted>"));
        assert!(joined.contains("health-hint: https://198.51.100.88/health"));
        assert!(joined.contains("http-proxy-listen: 0.0.0.0:8080"));
    }

    #[test]
    fn build_process_status_lines_suggests_recovery_for_stale_pid() {
        let config = Config::default();
        let lines = build_process_status_lines(&config, None, Ok(ProcessModeState::Stale(9999)))
            .expect("build process status");
        let joined = lines.join("\n");
        assert!(joined.contains("active: inactive (stale pid file)"));
        assert!(
            joined.contains("hint: stale pid file detected; `zodex restart` will cleanly recover")
        );
    }

    #[test]
    fn build_publisher_status_lines_includes_socket_and_run_user() {
        let config = Config::default();
        let lines = build_publisher_status_lines(&config, Ok(ProcessModeState::Running(5150)))
            .expect("build publisher status");
        let joined = lines.join("\n");
        assert!(joined.contains("service: zodex-prd"));
        assert!(joined.contains("run-user: zodex-publisher"));
        assert!(joined.contains("socket: /var/lib/zodex/publisher/run/zodex-prd.sock"));
        assert!(joined.contains("allowed-repos: 0"));
        assert!(joined.contains("hint: set `publisher_app_id` in config"));
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
    fn sprite_exec_verification_helpers_do_not_prepend_separator() {
        let source_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("bin")
            .join("zodex");
        let source = ["sprite_proxy.rs"]
            .into_iter()
            .map(|file| {
                std::fs::read_to_string(source_dir.join(file)).expect("read zodex source module")
            })
            .collect::<Vec<_>>()
            .join("\n");

        for fn_name in [
            "derive_remote_target_repo",
            "verify_local_sprite_health",
            "verify_agent_git_identity",
            "verify_reader_git_access",
            "verify_publisher_socket_permissions",
            "verify_publisher_key_isolation",
            "verify_sprite_health",
        ] {
            let start = source
                .find(&format!("fn {fn_name}"))
                .unwrap_or_else(|| panic!("missing function {fn_name}"));
            let tail = &source[start..];
            let end = tail.find("\nfn ").unwrap_or(tail.len());
            let body = &tail[..end];
            assert!(
                !body.contains("\"--\".to_string()"),
                "{fn_name} should not prepend `--` when building run_sprite_exec args"
            );
        }
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
        let config = Config::default();
        let lines = build_sprite_services_status_lines(
            &config,
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
        let config = Config::default();
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
            &config,
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

    #[test]
    fn sprite_service_supervisor_pids_from_ps_matches_sprite_managed_parents() {
        let raw = "\
11 sudo -n -u zodex-publisher /usr/local/bin/zodex-prd --config /etc/zodex/config.toml
12 sudo -n -u zodex-agent /usr/local/bin/zodexd --config /etc/zodex/config.toml
16 /usr/local/bin/zodex-prd --config /etc/zodex/config.toml
17 /usr/local/bin/zodexd --config /etc/zodex/config.toml
178 runuser -u zodex-publisher -- /usr/local/bin/zodex-prd --config /etc/zodex/config.toml
";
        let pids = sprite_service_supervisor_pids_from_ps(raw, Path::new("/etc/zodex/config.toml"));

        assert_eq!(pids.get(PUBLISHER_SERVICE_LABEL), Some(&11));
        assert_eq!(pids.get(SPRITE_MAIN_SERVICE_LABEL), Some(&12));
    }

    #[test]
    fn ensure_http_listener_ready_rejects_same_port_as_https() {
        let config = Config {
            bind_port: 443,
            http_bind_port: Some(443),
            ..Config::default()
        };

        let err = ensure_http_listener_ready_for_start(&config).expect_err("should fail");
        assert!(
            err.to_string()
                .contains("http_bind_port must differ from bind_port")
        );
    }

    #[test]
    fn build_status_summary_lines_notes_start_when_tls_files_missing() {
        let raw = "ActiveState=inactive\nSubState=dead\nUnitFileState=enabled\nExecMainStatus=1\n";
        let dir = tempdir().expect("tempdir");
        let config = Config {
            tls_cert_path: dir.path().join("missing-cert.pem").display().to_string(),
            tls_key_path: dir.path().join("missing-key.pem").display().to_string(),
            ..Config::default()
        };

        let lines = build_status_summary_lines(raw, &config, None);
        let joined = lines.join("\n");

        assert!(joined.contains("note: `zodex start` will create TLS artifacts automatically"));
    }

    #[test]
    fn tls_artifacts_exist_checks_both_files() {
        let dir = tempdir().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        fs::write(&cert, "cert").expect("write cert");

        let config = Config {
            tls_cert_path: cert.display().to_string(),
            tls_key_path: key.display().to_string(),
            ..Config::default()
        };
        assert!(!tls_artifacts_exist(&config));

        fs::write(&key, "key").expect("write key");
        assert!(tls_artifacts_exist(&config));
    }

    #[test]
    fn generate_self_signed_certificate_writes_pem_files() {
        let dir = tempdir().expect("tempdir");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        let config = Config {
            tls_cert_path: cert_path.display().to_string(),
            tls_key_path: key_path.display().to_string(),
            ..Config::default()
        };

        generate_self_signed_certificate(&config, IpAddr::V6(Ipv6Addr::LOCALHOST))
            .expect("generate self signed cert");

        let cert = fs::read_to_string(&cert_path).expect("read cert");
        let key = fs::read_to_string(&key_path).expect("read key");
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
    }
