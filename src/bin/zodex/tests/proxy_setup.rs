    #[cfg(unix)]
    fn write_fake_wrangler(
        dir: &Path,
        body: &str,
    ) -> (ProxyDeployCommandSpec, PathBuf) {
        let runner = dir.join("fake-wrangler");
        let staged_runner = dir.join("fake-wrangler.tmp");
        let log = dir.join("wrangler-calls.log");
        let log_literal = shell_escape_single_quotes(&log.display().to_string());
        let script = format!(
            "#!/bin/sh\nset -eu\nLOG={log_literal}\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n{body}\n"
        );
        // Linux can return ETXTBSY if a just-written executable is spawned
        // while the write handle is still being torn down. Stage the script,
        // close it explicitly, then atomically publish the executable path so
        // the command runner never observes a file that is open for writing.
        {
            use std::io::Write as _;

            let mut file = fs::File::create(&staged_runner).expect("create fake Wrangler");
            file.write_all(script.as_bytes()).expect("write fake Wrangler");
            file.sync_all().expect("sync fake Wrangler");
            file.set_permissions(fs::Permissions::from_mode(0o755))
                .expect("chmod fake Wrangler");
        }
        fs::rename(&staged_runner, &runner).expect("publish fake Wrangler");
        (
            ProxyDeployCommandSpec {
                program: "/bin/sh".to_string(),
                base_args: vec![runner.display().to_string()],
            },
            log,
        )
    }

    #[cfg(unix)]
    fn fake_wrangler_project(dir: &Path) -> PathBuf {
        let config = dir.join("wrangler.jsonc");
        fs::write(&config, "{}\n").expect("write fake config");
        config
    }

    #[test]
    fn cloudflare_account_selection_is_deterministic() {
        let accounts = vec![
            super::CloudflareAccount {
                id: "acct-a".to_string(),
                name: "Personal".to_string(),
            },
            super::CloudflareAccount {
                id: "acct-b".to_string(),
                name: "Team".to_string(),
            },
        ];
        let state = super::WranglerWhoamiState::Authenticated(accounts);

        let err = super::select_cloudflare_account(&state, None, None)
            .expect_err("multiple accounts must not be selected implicitly");
        assert!(err.to_string().contains("--cloudflare-account"));

        let by_name = super::select_cloudflare_account(&state, Some("team"), None)
            .expect("select by name")
            .expect("account");
        assert_eq!(by_name.id, "acct-b");

        let registered = super::select_cloudflare_account(&state, None, Some("acct-a"))
            .expect("select registered")
            .expect("account");
        assert_eq!(registered.name, "Personal");
    }

    #[test]
    fn registered_cloudflare_account_wins_over_ambient_fallback() {
        let accounts = vec![
            super::CloudflareAccount {
                id: "acct-registered".to_string(),
                name: "Registered".to_string(),
            },
            super::CloudflareAccount {
                id: "acct-ambient".to_string(),
                name: "Ambient".to_string(),
            },
        ];
        let state = super::WranglerWhoamiState::Authenticated(accounts);
        let fallback_account_id =
            super::cloudflare_account_fallback_id(Some("acct-registered"), Some("acct-ambient"));
        let selected = super::select_cloudflare_account(&state, None, fallback_account_id)
            .expect("registered fallback")
            .expect("account");
        assert_eq!(selected.id, "acct-registered");
        assert_eq!(
            super::cloudflare_account_fallback_id(None, Some("acct-ambient")),
            Some("acct-ambient")
        );
    }

    #[test]
    fn unauthenticated_account_name_is_not_guessed() {
        let err = super::select_cloudflare_account(
            &super::WranglerWhoamiState::Unauthenticated,
            Some("my-team"),
            None,
        )
        .expect_err("cannot resolve account name while unauthenticated");
        assert!(err.to_string().contains("wrangler login --use-keyring"));
    }

    #[cfg(unix)]
    #[test]
    fn fake_wrangler_one_account_permanent_deploy_uses_stable_account_id() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami)
    printf '%s\n' '{"loggedIn":true,"authType":"OAuth Token","accounts":[{"id":"acct-one","name":"Only Account"}]}'
    ;;
  deploy)
    test "${CLOUDFLARE_ACCOUNT_ID:-}" = "acct-one"
    cat > "$WRANGLER_OUTPUT_FILE_PATH" <<'EOF'
{"type":"wrangler-session","version":1,"wrangler_version":"4.120.0"}
{"type":"deploy","version":1,"worker_name":"zodex-dev","version_id":"v-perm","targets":["https://zodex-dev.example.workers.dev"]}
EOF
    printf 'presentation output\n'
    ;;
  --version)
    printf 'wrangler 4.120.0\n'
    ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let outcome = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            None,
            false,
            None,
        )
        .expect("permanent deploy");
        let super::CloudflareDeployOutcome::Permanent(permanent) = outcome else {
            panic!("expected permanent deploy");
        };
        assert_eq!(permanent.cloudflare_account_id, "acct-one");
        assert_eq!(permanent.metadata.version_id, "v-perm");
        assert_eq!(
            permanent.worker_url,
            "https://zodex-dev.example.workers.dev"
        );
        let calls = fs::read_to_string(log).expect("calls");
        assert!(calls.contains("whoami --json"));
        assert!(calls.contains("deploy --config"));
        assert!(!calls.contains("--temporary"));
    }

    #[cfg(unix)]
    #[test]
    fn fake_wrangler_multi_account_requires_or_resolves_explicit_choice() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami)
    printf '%s\n' '{"loggedIn":true,"authType":"OAuth Token","accounts":[{"id":"acct-a","name":"A"},{"id":"acct-b","name":"B"}]}'
    ;;
  deploy)
    test "${CLOUDFLARE_ACCOUNT_ID:-}" = "acct-b"
    cat > "$WRANGLER_OUTPUT_FILE_PATH" <<'EOF'
{"type":"wrangler-session","version":1,"wrangler_version":"4.120.0"}
{"type":"deploy","version":1,"worker_name":"zodex-dev","version_id":"v-b","targets":["https://zodex-dev.example.workers.dev"]}
EOF
    ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let err = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            None,
            false,
            None,
        )
        .expect_err("multi account must require a choice");
        assert!(err.to_string().contains("--cloudflare-account"));
        assert!(!fs::read_to_string(&log).expect("calls").contains("deploy"));

        fs::write(&log, "").expect("clear log");
        let outcome = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            Some("B"),
            None,
            false,
            None,
        )
        .expect("explicit account deploy");
        let super::CloudflareDeployOutcome::Permanent(permanent) = outcome else {
            panic!("expected permanent deploy");
        };
        assert_eq!(permanent.cloudflare_account_id, "acct-b");
    }

    #[cfg(unix)]
    #[test]
    fn fake_wrangler_unauthenticated_first_deploy_falls_back_once_to_temporary() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami)
    printf '%s\n' '{"loggedIn":false}' >&2
    exit 1
    ;;
  --version)
    printf 'wrangler 4.120.0\n'
    ;;
  deploy)
    case " $* " in
      *" --temporary "*)
        test -z "${CLOUDFLARE_ACCOUNT_ID:-}"
        cat > "$WRANGLER_OUTPUT_FILE_PATH" <<'EOF'
{"type":"wrangler-session","version":1,"wrangler_version":"4.120.0"}
{"type":"deploy","version":1,"worker_name":"zodex-temp","version_id":"v-temp","targets":["https://zodex-temp.preview.workers.dev"]}
EOF
        cat <<'EOF'
Temporary account ready:
  Claim within:   60 minutes
  Claim URL:      https://dash.cloudflare.com/claim-preview?claimToken=claim-secret-123
Deployed zodex-temp triggers
  https://zodex-temp.preview.workers.dev
EOF
        ;;
      *)
        cat >&2 <<'EOF'
To continue without logging in, rerun this command with `--temporary`.
Wrangler will use a temporary account and print a claim URL.
EOF
        exit 1
        ;;
    esac
    ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let outcome = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            None,
            false,
            None,
        )
        .expect("temporary fallback");
        let super::CloudflareDeployOutcome::Temporary(temporary) = outcome else {
            panic!("expected temporary deploy");
        };
        assert_eq!(
            temporary.worker_url,
            "https://zodex-temp.preview.workers.dev"
        );
        assert_eq!(
            temporary.claim_url,
            "https://dash.cloudflare.com/claim-preview?claimToken=claim-secret-123"
        );
        let calls = fs::read_to_string(log).expect("calls");
        assert!(calls.contains("deploy --config"));
        assert!(calls.contains("--version"));
        assert!(calls.contains("deploy --temporary --config"));
    }

    #[cfg(unix)]
    #[test]
    fn unrelated_wrangler_failure_never_triggers_temporary_deploy() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami) printf '%s\n' '{"loggedIn":false}' >&2; exit 1 ;;
  deploy) printf 'permission denied for account\n' >&2; exit 1 ;;
  --version) printf 'wrangler 4.120.0\n' ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let err = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            None,
            false,
            None,
        )
        .expect_err("unrelated failure");
        assert!(err.to_string().contains("permission denied"));
        let calls = fs::read_to_string(log).expect("calls");
        assert!(!calls.contains("--temporary"));
        assert!(!calls.contains("--version"));
    }

    #[cfg(unix)]
    #[test]
    fn authenticated_wrangler_failure_never_triggers_temporary_deploy() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami)
    printf '%s\n' '{"loggedIn":true,"authType":"OAuth Token","accounts":[{"id":"acct-one","name":"Only Account"}]}'
    ;;
  deploy)
    cat >&2 <<'EOF'
Permission denied while deploying this Worker.
To continue without logging in, rerun this command with `--temporary`.
EOF
    exit 1
    ;;
  --version) printf 'wrangler 4.120.0\n' ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let err = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            None,
            false,
            None,
        )
        .expect_err("authenticated deploy failure must stay permanent-only");
        assert!(err.to_string().contains("Permission denied"));
        let calls = fs::read_to_string(log).expect("calls");
        assert!(!calls.contains("--temporary"));
        assert!(!calls.contains("--version"));
    }

    #[cfg(unix)]
    #[test]
    fn registered_worker_never_falls_back_to_fresh_temporary_deploy() {
        let dir = tempdir().expect("tempdir");
        let config = fake_wrangler_project(dir.path());
        let body = r#"
case "${1:-}" in
  whoami) printf '%s\n' '{"loggedIn":false}' >&2; exit 1 ;;
  deploy)
    cat >&2 <<'EOF'
To continue without logging in, rerun this command with `--temporary`.
Wrangler will use a temporary account and print a claim URL.
EOF
    exit 1
    ;;
  --version) printf 'wrangler 4.120.0\n' ;;
  *) exit 97 ;;
esac
"#;
        let (runner, log) = write_fake_wrangler(dir.path(), body);
        let err = super::execute_cloudflare_deploy_flow(
            dir.path(),
            &config,
            &runner,
            None,
            Some("acct-existing"),
            true,
            None,
        )
        .expect_err("routine redeploy must require permanent auth");
        assert!(err.to_string().contains("wrangler login --use-keyring"));
        let calls = fs::read_to_string(log).expect("calls");
        assert!(!calls.contains("--temporary"));
        assert!(!calls.contains("--version"));
    }

    #[test]
    fn claim_and_mcp_secrets_are_redacted_from_generic_wrangler_errors() {
        let raw = concat!(
            "Claim URL: https://dash.cloudflare.com/claim-preview?claimToken=claim-super-secret\n",
            "MCP: https://worker.example/mcp?key=mcp-super-secret\n"
        );
        let safe = super::sanitize_wrangler_output(raw);
        assert!(!safe.contains("claim-super-secret"));
        assert!(!safe.contains("mcp-super-secret"));
        assert!(safe.contains("[REDACTED_CLOUDFLARE_CLAIM_URL]"));
        assert!(safe.contains("key=<redacted>"));
    }

    #[test]
    fn claim_url_parser_accepts_only_cloudflare_claim_preview_urls() {
        let claim = "https://dash.cloudflare.com/claim-preview?claimToken=abc123";
        assert_eq!(
            super::extract_cloudflare_claim_url(&format!("Claim URL: {claim}\n")),
            Some(claim.to_string())
        );
        assert_eq!(
            super::extract_cloudflare_claim_url(
                "https://evil.example/claim-preview?claimToken=abc123"
            ),
            None
        );
    }

    #[test]
    fn legacy_sprite_registry_migrates_without_losing_existing_records() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("sprites.json");
        fs::write(
            &path,
            r#"{
  "sprites": [{
    "name": "legacy",
    "org": "team",
    "remote_config": "/etc/zodex/config.toml",
    "last_setup_at": "2026-06-30T00:00:00Z"
  }]
}"#,
        )
        .expect("legacy registry");
        let registry = super::load_operator_sprite_registry_from_path(&path).expect("migrate");
        assert_eq!(registry.version, OPERATOR_SPRITES_REGISTRY_VERSION);
        assert_eq!(registry.sprites.len(), 1);
        assert_eq!(registry.sprites[0].name, "legacy");
        assert!(registry.sprites[0].proxy.is_none());

        super::save_operator_sprite_registry_to_path(&path, &registry).expect("save migrated");
        let saved = fs::read_to_string(path).expect("read migrated");
        assert!(saved.contains("\"version\": 2"));
    }

    #[test]
    fn setup_registry_refresh_preserves_non_secret_proxy_metadata() {
        let proxy = super::OperatorSpriteProxyRecord {
            cloudflare_account_id: "acct-123".to_string(),
            worker_name: "zodex-dev".to_string(),
            worker_url: "https://zodex-dev.example.workers.dev".to_string(),
            worker_version: "v1".to_string(),
            worker_build: "0.3.1-build".to_string(),
            deployed_at: "2026-08-17T01:00:00Z".to_string(),
        };
        let mut registry = OperatorSpriteRegistry::default();
        super::update_operator_sprite_setup_record(
            &mut registry,
            "dev",
            Some("team"),
            Path::new("/old/config.toml"),
            "2026-08-17T01:00:00Z",
        );
        super::update_operator_sprite_proxy_record(
            &mut registry,
            &ResolvedSprite {
                name: "dev".to_string(),
                org: Some("team".to_string()),
            },
            proxy.clone(),
        );
        super::update_operator_sprite_setup_record(
            &mut registry,
            "dev",
            Some("team"),
            Path::new("/new/config.toml"),
            "2026-08-17T02:00:00Z",
        );
        assert_eq!(registry.sprites[0].remote_config, "/new/config.toml");
        assert_eq!(registry.sprites[0].proxy.as_ref(), Some(&proxy));

        let serialized = serde_json::to_string(&registry).expect("serialize registry");
        assert!(!serialized.contains("claimToken"));
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("mcp?key="));
    }

    #[test]
    fn persisted_proxy_registry_contains_only_non_secret_cloudflare_metadata() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("sprites.json");
        let mut registry = OperatorSpriteRegistry::default();
        let sprite = ResolvedSprite {
            name: "dev".to_string(),
            org: Some("team".to_string()),
        };
        super::update_operator_sprite_proxy_record(
            &mut registry,
            &sprite,
            super::OperatorSpriteProxyRecord {
                cloudflare_account_id: "acct-safe".to_string(),
                worker_name: "zodex-dev".to_string(),
                worker_url: "https://zodex-dev.example.workers.dev".to_string(),
                worker_version: "worker-version-safe".to_string(),
                worker_build: "worker-build-safe".to_string(),
                deployed_at: "2026-08-17T03:00:00Z".to_string(),
            },
        );
        super::save_operator_sprite_registry_to_path(&path, &registry).expect("persist registry");
        let raw = fs::read_to_string(path).expect("registry contents");
        assert!(raw.contains("acct-safe"));
        assert!(raw.contains("zodex-dev.example.workers.dev"));
        for secret_marker in [
            "claimToken",
            "claim-url",
            "api_key",
            "mcp?key=",
            "CLOUDFLARE_API_TOKEN",
            "CLOUDFLARE_API_KEY",
        ] {
            assert!(
                !raw.contains(secret_marker),
                "persisted registry leaked {secret_marker}: {raw}"
            );
        }
    }

    #[test]
    fn sprite_connect_builds_encoded_capability_url_and_hides_it_after_clipboard_success() {
        let mut copied = String::new();
        let presentation = super::prepare_sprite_connect_presentation(
            "https://worker.example.workers.dev/",
            "key with & symbols",
            false,
            |value| {
                copied = value.to_string();
                true
            },
        )
        .expect("prepare connect");
        assert!(presentation.copied_to_clipboard);
        assert!(!presentation.print_capability_url);
        assert_eq!(copied, presentation.capability_url);
        let parsed = reqwest::Url::parse(&presentation.capability_url).expect("capability URL");
        assert_eq!(parsed.path(), "/mcp");
        assert_eq!(
            parsed
                .query_pairs()
                .find(|(key, _)| key == "key")
                .map(|(_, value)| value.into_owned()),
            Some("key with & symbols".to_string())
        );
    }

    #[test]
    fn sprite_connect_parses_remote_key_without_guest_python_dependency() {
        let raw = concat!(
            "bind_host = \"0.0.0.0\"\n",
            "api_key = \"secret-key-value\"\n",
            "service_port = 8080\n"
        );
        assert_eq!(
            super::parse_remote_sprite_mcp_key(raw).expect("parse remote key"),
            "secret-key-value"
        );
        let err = super::parse_remote_sprite_mcp_key("service_port = 8080\n")
            .expect_err("missing key");
        assert!(err.to_string().contains("missing api_key"));
    }

    #[test]
    fn sprite_connect_prints_secret_only_on_explicit_show_or_clipboard_failure() {
        let failed = super::prepare_sprite_connect_presentation(
            "https://worker.example.workers.dev",
            "secret",
            false,
            |_| false,
        )
        .expect("failed clipboard presentation");
        assert!(failed.print_capability_url);

        let shown = super::prepare_sprite_connect_presentation(
            "https://worker.example.workers.dev",
            "secret",
            true,
            |_| true,
        )
        .expect("show URL presentation");
        assert!(shown.print_capability_url);
    }

    #[test]
    fn sprite_connect_rejects_worker_bound_to_another_sprite_before_key_use() {
        let status = ProxyWorkerStatus {
            component: "zodex-cloudflare-worker".to_string(),
            build: "build-a".to_string(),
            sprite_origin: Some("https://other.example.sprites.app".to_string()),
        };
        let err = super::validate_sprite_connect_worker(
            &status,
            "build-a",
            "https://selected.example.sprites.app",
        )
        .expect_err("mismatched Sprite origin must fail");
        assert!(err.to_string().contains("different Sprite origin"));

        let matching = ProxyWorkerStatus {
            sprite_origin: Some("https://selected.example.sprites.app".to_string()),
            ..status
        };
        super::validate_sprite_connect_worker(
            &matching,
            "build-a",
            "https://selected.example.sprites.app",
        )
        .expect("matching Worker");
    }
