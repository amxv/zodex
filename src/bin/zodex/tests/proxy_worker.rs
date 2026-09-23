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
    fn render_proxy_wrangler_config_replaces_all_embedded_placeholders() {
        let rendered = render_proxy_wrangler_config(
            "https://zodex.example.sprites.app",
            "zodex-dev-1234567890",
            "0.3.1-testbuild",
        )
        .expect("render");
        assert!(rendered.contains("https://zodex.example.sprites.app"));
        assert!(rendered.contains("zodex-dev-1234567890"));
        assert!(rendered.contains("0.3.1-testbuild"));
        assert!(!rendered.contains("__SPRITE_ORIGIN__"));
        assert!(!rendered.contains("__WORKER_NAME__"));
        assert!(!rendered.contains("__ZODEX_WORKER_BUILD__"));
        assert!(!rendered.contains("DOCS_ORIGIN"));
    }

    #[test]
    fn current_sprite_cli_args_use_info_and_config_update() {
        assert_eq!(
            sprite_info_args("dev", Some("acme")),
            vec!["info", "--sprite", "dev", "--org", "acme"]
        );
        assert_eq!(
            sprite_config_url_auth_args("dev", Some("acme"), "public"),
            vec![
                "config", "update", "--sprite", "dev", "--url-auth", "public", "--org",
                "acme"
            ]
        );
    }

    #[test]
    fn parse_sprite_info_accepts_current_and_legacy_labels() {
        let current = parse_sprite_info(
            "Name: dev\nURL: https://dev.example.sprites.app\nURL Auth: public\n",
        );
        assert_eq!(current.url.as_deref(), Some("https://dev.example.sprites.app"));
        assert_eq!(current.auth.as_deref(), Some("public"));

        let legacy = parse_sprite_info(
            "URL: https://dev.example.sprites.app\nAuth: sprite\n",
        );
        assert_eq!(legacy.auth.as_deref(), Some("sprite"));
    }

    #[test]
    fn derived_worker_names_are_stable_bounded_and_collision_resistant() {
        let resolution = ProxyOriginResolution {
            origin: "https://dev.example.sprites.app".to_string(),
            sprite_url_auth: Some("public".to_string()),
            sprite: Some(ResolvedSprite {
                name: "A very long Sprite name with punctuation and enough characters to force truncation"
                    .to_string(),
                org: Some("Example Org".to_string()),
            }),
        };
        let first = derive_proxy_worker_name(&resolution);
        let second = derive_proxy_worker_name(&resolution);
        assert_eq!(first, second);
        assert!(first.len() <= 63);
        assert!(!first.starts_with('-'));
        assert!(!first.ends_with('-'));
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );

        let mut other = resolution.clone();
        other.sprite.as_mut().expect("sprite").name.push('2');
        assert_ne!(first, derive_proxy_worker_name(&other));
    }

    #[test]
    fn embedded_worker_materializes_without_reading_source_tree_at_runtime() {
        let dir = tempdir().expect("tempdir");
        let resolution = ProxyOriginResolution {
            origin: "https://dev.example.sprites.app".to_string(),
            sprite_url_auth: Some("public".to_string()),
            sprite: None,
        };
        let build = proxy_worker_build_id();
        let config = materialize_proxy_project(
            dir.path(),
            &resolution,
            "zodex-dev-1234567890",
            &build,
        )
        .expect("materialize");
        assert!(dir.path().join("src/index.js").is_file());
        let rendered = fs::read_to_string(config).expect("config");
        assert!(rendered.contains(&build));
        assert!(rendered.contains("https://dev.example.sprites.app"));
    }

    #[test]
    fn parses_documented_wrangler_structured_deploy_output() {
        let raw = concat!(
            "{\"type\":\"wrangler-session\",\"version\":1,\"wrangler_version\":\"4.120.0\"}\n",
            "{\"type\":\"deploy\",\"version\":1,\"worker_name\":\"zodex-dev\",\"version_id\":\"v1-abc\",\"targets\":[\"https://zodex-dev.example.workers.dev\"]}\n"
        );
        let parsed = parse_wrangler_deploy_output(raw).expect("parse deploy");
        assert_eq!(parsed.worker_name, "zodex-dev");
        assert_eq!(parsed.version_id, "v1-abc");
        assert_eq!(parsed.wrangler_version.as_deref(), Some("4.120.0"));
        assert_eq!(
            parsed.targets,
            vec!["https://zodex-dev.example.workers.dev"]
        );
    }

    #[test]
    fn wrangler_version_floor_matches_temporary_deployment_contract() {
        assert_eq!(parse_wrangler_version("wrangler 4.120.0"), Some((4, 120, 0)));
        assert_eq!(
            parse_wrangler_version("⛅️ wrangler 4.102.0-beta.1"),
            Some((4, 102, 0))
        );
        ensure_temporary_wrangler_version("wrangler 4.102.0").expect("minimum supported");
        let err = ensure_temporary_wrangler_version("wrangler 4.101.9")
            .expect_err("older Wrangler must fail");
        assert!(err.to_string().contains(">=4.102.0"));
    }

    #[test]
    fn wrangler_structured_failure_is_not_mistaken_for_deploy_metadata() {
        let raw = concat!(
            "{\"type\":\"wrangler-session\",\"version\":1,\"wrangler_version\":\"4.120.0\"}\n",
            "{\"type\":\"command-failed\",\"version\":1,\"error\":{\"message\":\"not authenticated\"}}\n"
        );
        let err = parse_wrangler_deploy_output(raw).expect_err("command failure");
        assert!(err.to_string().contains("not authenticated"));
    }

    #[test]
    fn worker_status_distinguishes_current_stale_and_foreign_deployments() {
        let current = ProxyWorkerStatus {
            component: "zodex-cloudflare-worker".to_string(),
            build: "build-a".to_string(),
            sprite_origin: Some("https://dev.example.sprites.app".to_string()),
        };
        assert_eq!(proxy_worker_build_state(&current, "build-a"), "current");
        assert_eq!(proxy_worker_build_state(&current, "build-b"), "stale");

        let foreign = ProxyWorkerStatus {
            component: "some-other-worker".to_string(),
            ..current
        };
        assert_eq!(proxy_worker_build_state(&foreign, "build-a"), "foreign");
    }

    #[test]
    fn proxy_origin_requires_public_sprite_auth_when_known() {
        let public = ProxyOriginResolution {
            origin: "https://dev.example.sprites.app".to_string(),
            sprite_url_auth: Some("public".to_string()),
            sprite: None,
        };
        ensure_proxy_origin_is_publicly_routable(&public).expect("public origin");

        let private = ProxyOriginResolution {
            sprite_url_auth: Some("sprite".to_string()),
            ..public
        };
        let err = ensure_proxy_origin_is_publicly_routable(&private).expect_err("private origin");
        assert!(err.to_string().contains("sprite config update --url-auth public"));
    }

    #[cfg(unix)]
    #[test]
    fn fake_wrangler_runner_uses_structured_output_and_explicit_account() {
        let dir = tempdir().expect("tempdir");
        let runner = dir.path().join("fake-wrangler");
        fs::write(
            &runner,
            r#"#!/bin/sh
set -eu
test "${CLOUDFLARE_ACCOUNT_ID:-}" = "acct-test"
cat > "$WRANGLER_OUTPUT_FILE_PATH" <<'EOF'
{"type":"wrangler-session","version":1,"wrangler_version":"4.120.0"}
{"type":"deploy","version":1,"worker_name":"zodex-dev","version_id":"v-test","targets":["https://zodex-dev.example.workers.dev"]}
EOF
printf 'human presentation output\n'
"#,
        )
        .expect("write runner");
        fs::set_permissions(&runner, fs::Permissions::from_mode(0o755)).expect("chmod runner");
        let config = dir.path().join("wrangler.jsonc");
        fs::write(&config, "{}").expect("config");
        let result = execute_wrangler_deploy(
            &ProxyDeployCommandSpec {
                program: "/bin/sh".to_string(),
                base_args: vec![runner.display().to_string()],
            },
            dir.path(),
            &config,
            Some("acct-test"),
        )
        .expect("fake deploy");
        assert_eq!(result.metadata.version_id, "v-test");
        assert_eq!(result.metadata.worker_name, "zodex-dev");
        assert_eq!(result.human_output, "human presentation output\n");
    }

    #[cfg(unix)]
    #[test]
    fn fake_wrangler_runner_surfaces_structured_deploy_errors() {
        let dir = tempdir().expect("tempdir");
        let runner = dir.path().join("fake-wrangler-error");
        fs::write(
            &runner,
            r#"#!/bin/sh
set -eu
cat > "$WRANGLER_OUTPUT_FILE_PATH" <<'EOF'
{"type":"wrangler-session","version":1,"wrangler_version":"4.120.0"}
{"type":"command-failed","version":1,"error":{"message":"permission denied for account"}}
EOF
printf 'ordinary stderr detail\n' >&2
exit 1
"#,
        )
        .expect("write runner");
        fs::set_permissions(&runner, fs::Permissions::from_mode(0o755)).expect("chmod runner");
        let config = dir.path().join("wrangler.jsonc");
        fs::write(&config, "{}").expect("config");
        let err = execute_wrangler_deploy(
            &ProxyDeployCommandSpec {
                program: "/bin/sh".to_string(),
                base_args: vec![runner.display().to_string()],
            },
            dir.path(),
            &config,
            None,
        )
        .expect_err("fake deploy should fail");
        let message = err.to_string();
        assert!(message.contains("permission denied for account"));
        assert!(message.contains("ordinary stderr detail"));
    }

    #[test]
    fn proxy_mcp_status_looks_healthy_accepts_auth_or_success() {
        assert!(proxy_mcp_status_looks_healthy(200));
        assert!(proxy_mcp_status_looks_healthy(401));
        assert!(!proxy_mcp_status_looks_healthy(404));
    }
