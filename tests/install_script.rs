use std::path::PathBuf;
use std::process::Command;

fn install_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("install.sh")
}

#[test]
fn install_script_has_expected_structure() {
    let script = std::fs::read_to_string(install_script_path()).expect("read install script");

    let required_snippets = [
        "set -euo pipefail",
        "ZODEX_VERSION",
        "ZODEX_ASSET_URL",
        "ZODEX_BINARY_SOURCE_DIR",
        "ZODEX_INSTALL_DIR",
        "ZODEX_CONFIG_PATH",
        "ZODEX_AGENT_USER",
        "ZODEX_AGENT_HOME",
        "ZODEX_AGENT_SHELL",
        "ZODEX_DEFAULT_WORKDIR",
        "ZODEX_PUBLISHER_USER",
        "ZODEX_PUBLISHER_HOME",
        "ZODEX_SERVICE_GROUP",
        "ZODEX_GIT_USER_NAME",
        "ZODEX_GIT_USER_EMAIL",
        "ZODEX_READER_KEY_DIR",
        "ZODEX_HTTP_BIND_PORT",
        "ZODEX_PUBLIC_HOST",
        "ensure_service_accounts()",
        "detect_platform()",
        "resolved_http_bind_port()",
        "resolved_public_host()",
        "install_runtime_prerequisites()",
        "install_build_prerequisites()",
        "resolve_release_asset_url()",
        "server_archive_name=\"zodex-${TARGET_TRIPLE}.tar.gz\"",
        "install_binaries_from_release()",
        "install_binaries_from_source()",
        "run_cli_install()",
        "configure_agent_git_identity()",
        "configure_agent_git_reader_helper()",
        "git config --global user.name",
        "git config --global user.email",
        "${ZODEX_STATE_DIR}/publisher/run",
        "${ZODEX_STATE_DIR}/publisher/logs",
        "credential.https://github.com.helper",
        "git-credential-helper",
        "print_next_steps()",
        "apt-get install -y --no-install-recommends",
        "build-essential pkg-config libssl-dev git",
        "zodex-prd",
        "agent_home = \"${ZODEX_AGENT_HOME}\"",
        "default_workdir = \"${ZODEX_DEFAULT_WORKDIR}\"",
        "The commands below assume the default config path",
        "Most installs can keep the built-in defaults.",
        "reader_app_id",
        "reader_installation_id",
        "publisher_client_id",
        "enable Device Flow on the push-grant GitHub App",
        "rotate the installer-generated API key",
        "curl -k \"https://${public_host}/health\"",
        "MCP URL shape: https://${public_host}/mcp?key=<redacted>",
        "credential.https://github.com.useHttpPath true",
    ];

    for snippet in required_snippets {
        assert!(
            script.contains(snippet),
            "install script missing snippet: {snippet}"
        );
    }
}

#[test]
fn install_script_does_not_use_generic_target_triple_tarball_match() {
    let script = std::fs::read_to_string(install_script_path()).expect("read install script");

    assert!(
        !script.contains("${TARGET_TRIPLE}[^\"]*\\.tar\\.gz"),
        "install script should not select release assets via generic target triple tarball match"
    );
    let deprecated_platform_name = ["run", "pod"].concat();
    assert!(
        !script.contains(&deprecated_platform_name),
        "install script should not contain deprecated platform-specific branches"
    );
}

#[test]
fn install_script_is_valid_bash_syntax() {
    let output = Command::new("bash")
        .arg("-n")
        .arg(install_script_path())
        .output()
        .expect("run bash -n");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("bash -n failed: {stderr}");
    }
}
