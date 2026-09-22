use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use zodex::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    ProviderCallMetadata,
};
use zodex::local::{LocalHistoryRuntime, LocalHistoryRuntimeConfig, PRESENTATION_SCHEMA_VERSION};

#[test]
fn zodex_root_rejects_removed_commands_and_global_config() {
    for args in [
        vec!["install"],
        vec!["start"],
        vec!["stop"],
        vec!["restart"],
        vec!["status"],
        vec!["logs"],
        vec!["set-key", "secret"],
        vec!["rotate-key"],
        vec!["show-url"],
        vec!["tls", "setup"],
        vec!["publisher", "status"],
        vec!["proxy", "status"],
        vec!["github", "status"],
        vec!["--config", "/etc/zodex/config.toml", "sprite", "status"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
            .args(&args)
            .output()
            .unwrap_or_else(|error| panic!("run zodex {args:?}: {error}"));
        assert!(
            !output.status.success(),
            "removed root syntax unexpectedly succeeded: zodex {args:?}"
        );
    }
}

#[test]
fn zodex_local_first_run_status_json_is_versioned_and_unconfigured() {
    let fixture = LocalCliFixture::new();
    let output = fixture
        .command()
        .args(["local", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(value["schema_version"], 3);
    assert_eq!(value["configured"], false);
    assert_eq!(value["state"], "unconfigured");
    assert_eq!(value["history"]["max_age"], "60d");
    assert_eq!(value["history"]["max_age_seconds"], 60 * 24 * 60 * 60);
    assert_eq!(value["history"]["max_size"], "500mb");
    assert_eq!(value["history"]["max_size_bytes"], 500 * 1024 * 1024u64);
    assert!(value["discovery"].is_null());
    assert!(
        value["discovery_path"]
            .as_str()
            .unwrap()
            .ends_with("/state/zodex/local/runtime/discovery.json")
    );
}

#[test]
fn zodex_local_config_set_get_persists_non_secret_values() {
    let fixture = LocalCliFixture::new();
    for (key, value) in [
        ("history.max-age", "2d"),
        ("history.max-size", "1gb"),
        ("context.repo-agents", "false"),
        ("context.repo-skills", "false"),
        ("context.skills.codex", "false"),
    ] {
        let output = fixture
            .command()
            .args(["local", "config", "set", key, value])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = fixture
            .command()
            .args(["local", "config", "get", key])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), value);
    }

    let config_path = fixture.config_root.join("zodex/local.toml");
    let raw = fs::read_to_string(config_path).unwrap();
    assert!(raw.contains("max_age = \"2d\""));
    assert!(raw.contains("max_size = \"1gb\""));
    assert!(raw.contains("repo_agents = false"));
    assert!(raw.contains("codex = false"));
    assert!(!raw.contains("api_key"));
    assert!(!raw.contains("runtime_key"));
}

#[test]
fn zodex_local_config_rejects_invalid_retention_without_overwriting_config() {
    let fixture = LocalCliFixture::new();
    let seed = fixture
        .command()
        .args(["local", "config", "set", "history.max-age", "2d"])
        .output()
        .unwrap();
    assert!(seed.status.success());

    for (key, value) in [
        ("history.max-age", "forever"),
        ("history.max-age", "0h"),
        ("history.max-size", "huge"),
        ("history.max-size", "0mb"),
    ] {
        let output = fixture
            .command()
            .args(["local", "config", "set", key, value])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{key}={value} should be rejected");
    }

    let output = fixture
        .command()
        .args(["local", "config", "get", "history.max-age"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2d");
}

#[test]
fn zodex_local_config_set_rejects_active_runtime_state_with_stop_hint() {
    let fixture = LocalCliFixture::new();
    let runtime_dir = fixture.state_root.join("zodex/local/runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(
        runtime_dir.join("state.json"),
        r#"{"schema_version":1,"runtime_id":"test","lifecycle":"ready"}"#,
    )
    .unwrap();

    let output = fixture
        .command()
        .args(["local", "config", "set", "history.max-age", "2d"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("zodex local stop"), "{stderr}");
}

#[test]
fn zodex_local_context_config_can_change_while_runtime_is_active() {
    let fixture = LocalCliFixture::new();
    let runtime_dir = fixture.state_root.join("zodex/local/runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(
        runtime_dir.join("state.json"),
        r#"{"schema_version":1,"runtime_id":"test","lifecycle":"ready"}"#,
    )
    .unwrap();

    let output = fixture
        .command()
        .args(["local", "config", "set", "context.enabled", "false"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = fixture
        .command()
        .args(["local", "config", "get", "context.enabled"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "false");
}

#[test]
fn zodex_local_history_queries_exact_offline_evidence_and_clear_removes_store() {
    let fixture = LocalCliFixture::new();
    let database = fixture
        .state_root
        .join("zodex/local/history/history.sqlite3");
    let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        database.clone(),
        "operator-cli-history-test",
        365 * 24 * 60 * 60,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let invocation = history
        .begin(
            InvocationContext::default().with_provider(ProviderCallMetadata::new(
                "openai/session",
                "operator-cli-provider-session",
            )),
            InvocationStart::new(
                "apply_patch",
                serde_json::json!({
                    "patch":"*** Begin Patch\n*** Add File: cli-history.txt\n+history\n*** End Patch\n",
                    "workdir":fixture.home
                }),
            ),
        )
        .unwrap();
    let invocation_id = invocation.invocation_id.unwrap();
    let agent_id = invocation.agent_id.as_deref().unwrap().to_string();
    history
        .complete(
            &invocation,
            InvocationOutcome::Success(serde_json::json!({"output":"exact-handler-result"})),
        )
        .unwrap();
    history.shutdown_blocking().unwrap();

    let output = fixture
        .command()
        .args([
            "local",
            "history",
            "--agent",
            &agent_id,
            "--workdir",
            fixture.home.to_str().unwrap(),
            "--format",
            "json",
            "--raw",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["id"], invocation_id);
    assert_eq!(records[0]["agent_id"], agent_id);
    assert_eq!(
        records[0]["arguments"]["workdir"].as_str(),
        fixture.home.to_str()
    );
    assert_eq!(
        records[0]["result"],
        serde_json::json!({"output":"exact-handler-result"})
    );

    let output = fixture
        .command()
        .args(["local", "history", "--agent", &agent_id, "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let presentation: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(presentation["schema_version"], PRESENTATION_SCHEMA_VERSION);
    assert_eq!(presentation["agents"][0]["id"], agent_id);
    assert_eq!(presentation["records"][0]["agent_id"], agent_id);
    assert_eq!(presentation["records"][0]["kind"], "generic");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("operator-cli-provider-session"),
        "normalized JSON must not expose provider correlation keys"
    );

    let output = fixture
        .command()
        .args(["local", "history", "--agent", &agent_id])
        .output()
        .unwrap();
    assert!(output.status.success());
    let markdown = String::from_utf8_lossy(&output.stdout);
    assert!(
        markdown.contains(&format!("Agent `{agent_id}`")),
        "{markdown}"
    );
    assert!(markdown.contains("**apply_patch**"), "{markdown}");

    let output = fixture
        .command()
        .args([
            "local",
            "history",
            "--id",
            &invocation_id.to_string(),
            "--format",
            "json",
            "--raw",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let detail: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(detail[0]["id"], invocation_id);
    assert_eq!(
        detail[0]["provider_session_key"],
        "operator-cli-provider-session"
    );

    let output = fixture
        .command()
        .args(["local", "history", "clear"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requires --yes"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let runtime_dir = fixture.state_root.join("zodex/local/runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(runtime_dir.join("state.json"), "active").unwrap();
    let output = fixture
        .command()
        .args(["local", "history", "clear", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("zodex local stop"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(runtime_dir).unwrap();

    let output = fixture
        .command()
        .args(["local", "history", "clear", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!database.exists());
    assert!(!std::path::PathBuf::from(format!("{}-wal", database.display())).exists());
    assert!(!std::path::PathBuf::from(format!("{}-shm", database.display())).exists());

    let output = fixture
        .command()
        .args(["local", "history", "--last", "20"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "No Local history.\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn zodex_local_runtime_actions_fail_cleanly_on_unsupported_host() {
    let fixture = LocalCliFixture::new();
    let output = fixture.command().args(["local", "start"]).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("supported on macOS and Windows"),
        "{stderr}"
    );
    assert!(stderr.contains("zodex local status"), "{stderr}");
}

struct LocalCliFixture {
    _root: TempDir,
    home: std::path::PathBuf,
    config_root: std::path::PathBuf,
    data_root: std::path::PathBuf,
    state_root: std::path::PathBuf,
}

impl LocalCliFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let config_root = root.path().join("config");
        let data_root = root.path().join("data");
        let state_root = root.path().join("state");
        for path in [&home, &config_root, &data_root, &state_root] {
            fs::create_dir_all(path).unwrap();
        }
        Self {
            _root: root,
            home,
            config_root,
            data_root,
            state_root,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zodex"));
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("XDG_DATA_HOME", &self.data_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .current_dir(Path::new(&self.home));
        command
    }
}
