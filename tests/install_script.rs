use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

fn install_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("install.sh")
}

#[test]
fn installer_auto_mode_is_always_operator_and_runtime_is_explicit() {
    let command = r#"
eval "$(sed -n '/^resolved_install_mode()/,/^}/p' "${INSTALL_SCRIPT}")"
ZODEX_INSTALL_MODE=auto
printf 'auto=%s\n' "$(resolved_install_mode)"
ZODEX_INSTALL_MODE=operator
printf 'operator=%s\n' "$(resolved_install_mode)"
ZODEX_INSTALL_MODE=runtime
printf 'runtime=%s\n' "$(resolved_install_mode)"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .output()
        .expect("resolve installer modes");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "auto=operator\noperator=operator\nruntime=runtime\n"
    );
}

#[test]
fn operator_platform_supports_apple_silicon_macos_and_rejects_intel_macos() {
    let script_path = install_script_path();
    let arm64 = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
eval "$(sed -n '/^detect_operator_platform()/,/^}/p' "${INSTALL_SCRIPT}")"
log() { :; }
die() { printf '%s\n' "$*" >&2; exit 1; }
uname() {
  case "$1" in
    -s) printf 'Darwin\n' ;;
    -m) printf 'arm64\n' ;;
  esac
}
detect_operator_platform
printf '%s\n' "${TARGET_TRIPLE}"
"#,
        )
        .env("INSTALL_SCRIPT", &script_path)
        .output()
        .expect("detect Apple Silicon macOS operator platform");
    assert!(arm64.status.success());
    assert_eq!(
        String::from_utf8_lossy(&arm64.stdout),
        "aarch64-apple-darwin\n"
    );

    let intel = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
eval "$(sed -n '/^detect_operator_platform()/,/^}/p' "${INSTALL_SCRIPT}")"
log() { :; }
die() { printf '%s\n' "$*" >&2; exit 1; }
uname() {
  case "$1" in
    -s) printf 'Darwin\n' ;;
    -m) printf 'x86_64\n' ;;
  esac
}
detect_operator_platform
"#,
        )
        .env("INSTALL_SCRIPT", &script_path)
        .output()
        .expect("reject Intel macOS operator platform");
    assert!(!intel.status.success());
    assert!(
        String::from_utf8_lossy(&intel.stderr)
            .contains("Apple Silicon macOS only; Intel macOS is unsupported")
    );
}

#[test]
fn operator_release_urls_are_deterministic_and_versions_accept_optional_v_prefix() {
    let command = r#"
eval "$(sed -n '/^normalize_release_version()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^resolve_release_asset_url()/,/^}/p' "${INSTALL_SCRIPT}")"
ZODEX_REPO=amxv/zodex
ZODEX_ASSET_URL=
TARGET_TRIPLE=aarch64-apple-darwin
for version in latest 0.3.4 v0.3.4; do
  ZODEX_VERSION="$version"
  resolve_release_asset_url
done
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .output()
        .expect("resolve deterministic operator release URLs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "https://github.com/amxv/zodex/releases/latest/download/zodex-aarch64-apple-darwin.tar.gz\n",
            "https://github.com/amxv/zodex/releases/download/v0.3.4/zodex-aarch64-apple-darwin.tar.gz\n",
            "https://github.com/amxv/zodex/releases/download/v0.3.4/zodex-aarch64-apple-darwin.tar.gz\n",
        )
    );
}

#[test]
fn release_downloads_retry_and_abort_stalled_transfers() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-download-retry-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&test_dir).expect("create download retry directory");
    let count_file = test_dir.join("count");
    let args_file = test_dir.join("args");
    let destination = test_dir.join("archive");
    std::fs::write(&count_file, "0\n").expect("seed download retry count");

    let command = r#"
eval "$(sed -n '/^download_file()/,/^}/p' "${INSTALL_SCRIPT}")"
warn() { :; }
sleep() { :; }
curl() {
  local count
  count="$(cat "${COUNT_FILE}")"
  count=$((count + 1))
  printf '%s\n' "$count" > "${COUNT_FILE}"
  printf '%s\n' "$*" >> "${ARGS_FILE}"
  local destination=""
  while (($#)); do
    if [[ "$1" == "-o" ]]; then
      shift
      destination="$1"
    fi
    shift
  done
  if (( count < 3 )); then
    return 28
  fi
  printf 'downloaded\n' > "$destination"
}
download_file 'https://example.invalid/archive' "${DESTINATION}"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .env("COUNT_FILE", &count_file)
        .env("ARGS_FILE", &args_file)
        .env("DESTINATION", &destination)
        .output()
        .expect("exercise bounded release download retries");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&count_file).expect("read retry count"),
        "3\n"
    );
    assert_eq!(
        std::fs::read_to_string(&destination).expect("read downloaded fixture"),
        "downloaded\n"
    );
    let args = std::fs::read_to_string(&args_file).expect("read curl arguments");
    for expected in [
        "--connect-timeout 15",
        "--speed-limit 1024",
        "--speed-time 20",
        "--max-time 600",
    ] {
        assert!(
            args.contains(expected),
            "missing curl guard {expected}: {args}"
        );
    }

    std::fs::remove_dir_all(&test_dir).expect("remove download retry directory");
}

#[test]
fn runtime_config_migration_updates_only_the_known_legacy_bundle_default() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-runtime-config-migration-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&test_dir).expect("create migration test directory");
    let legacy_config = test_dir.join("legacy.toml");
    let partially_migrated_config = test_dir.join("partially-migrated.toml");
    let custom_config = test_dir.join("custom.toml");
    std::fs::write(
        &legacy_config,
        concat!(
            "api_key = \"redacted\"\n",
            "bind_port = 8443\n",
            "http_bind_port = 9090\n",
            "tls_mode = \"manual\"\n",
            "publisher_max_bundle_bytes = 33554432\n",
        ),
    )
    .expect("write legacy config");
    std::fs::write(
        &partially_migrated_config,
        concat!(
            "api_key = \"redacted\"\n",
            "service_port = 8080\n",
            "publisher_max_bundle_bytes = 33554432\n",
            "# BEGIN ZODEX_GH_APPS_MANAGED\n",
            "reader_app_id = 1\n",
            "publisher_app_id = 2\n",
            "# END ZODEX_GH_APPS_MANAGED\n",
        ),
    )
    .expect("write partially migrated config");
    std::fs::write(
        &custom_config,
        concat!(
            "api_key = \"redacted\"\n",
            "service_port = 7070\n",
            "publisher_max_bundle_bytes = 33554432\n",
            "# BEGIN ZODEX_GH_APPS_MANAGED\n",
            "publisher_client_id = \"Iv1.current\"\n",
            "# END ZODEX_GH_APPS_MANAGED\n",
        ),
    )
    .expect("write custom config");

    let command = r#"
eval "$(sed -n '/^migrate_runtime_config()/,/^}/p' "${INSTALL_SCRIPT}")"
log() { :; }
die() { printf '%s\n' "$*" >&2; exit 1; }
ZODEX_SERVICE_PORT=8080
ZODEX_DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES=134217728
ZODEX_CONFIG_PATH="${LEGACY_CONFIG}"
migrate_runtime_config
ZODEX_CONFIG_PATH="${PARTIALLY_MIGRATED_CONFIG}"
migrate_runtime_config
ZODEX_CONFIG_PATH="${CUSTOM_CONFIG}"
migrate_runtime_config
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .env("LEGACY_CONFIG", &legacy_config)
        .env("PARTIALLY_MIGRATED_CONFIG", &partially_migrated_config)
        .env("CUSTOM_CONFIG", &custom_config)
        .output()
        .expect("run runtime config migration");
    if !output.status.success() {
        panic!(
            "runtime config migration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let migrated = std::fs::read_to_string(&legacy_config).expect("read migrated config");
    assert!(migrated.contains("service_port = 9090"));
    assert!(migrated.contains("publisher_max_bundle_bytes = 134217728"));
    assert!(!migrated.contains("bind_port ="));
    assert!(!migrated.contains("http_bind_port ="));
    assert!(!migrated.contains("tls_mode ="));

    let partially_migrated = std::fs::read_to_string(&partially_migrated_config)
        .expect("read partially migrated config");
    assert!(partially_migrated.contains("service_port = 8080"));
    assert!(partially_migrated.contains("publisher_max_bundle_bytes = 134217728"));
    assert!(partially_migrated.contains("# BEGIN ZODEX_GH_APPS_MANAGED"));

    let custom = std::fs::read_to_string(&custom_config).expect("read custom config");
    assert!(custom.contains("service_port = 7070"));
    assert!(custom.contains("publisher_max_bundle_bytes = 33554432"));

    std::fs::remove_dir_all(&test_dir).expect("remove migration test directory");
}

#[test]
fn runtime_binary_install_never_installs_operator_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-runtime-install-test-{}-{unique}",
        std::process::id()
    ));
    let source_dir = test_dir.join("source");
    let install_dir = test_dir.join("install");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    std::fs::create_dir_all(&install_dir).expect("create install directory");

    for binary in [
        "zodex",
        "zodex-agent",
        "git-remote-zodex",
        "zodexd",
        "zodex-prd",
    ] {
        let path = source_dir.join(binary);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fixture binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path)
                .expect("read fixture metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("make fixture executable");
        }
    }
    std::fs::write(install_dir.join("zodex"), "stale operator\n")
        .expect("write stale operator binary");

    let command = r#"
eval "$(sed -n '/^install_binaries_from_dir()/,/^}/p' "${INSTALL_SCRIPT}")"
die() { printf '%s\n' "$*" >&2; exit 1; }
install_binaries_from_dir "${SOURCE_DIR}"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .env("SOURCE_DIR", &source_dir)
        .env("ZODEX_INSTALL_DIR", &install_dir)
        .output()
        .expect("install runtime fixture binaries");
    if !output.status.success() {
        panic!(
            "runtime fixture install failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(!install_dir.join("zodex").exists());
    for binary in ["zodex-agent", "git-remote-zodex", "zodexd", "zodex-prd"] {
        assert!(install_dir.join(binary).is_file(), "missing {binary}");
    }

    std::fs::remove_dir_all(&test_dir).expect("remove runtime install test directory");
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

#[test]
fn operator_installer_signs_adhoc_macho_with_available_local_identity() {
    let command = r#"
eval "$(sed -n '/^resolve_operator_codesign_identity()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^sign_local_operator_if_configured()/,/^}/p' "${INSTALL_SCRIPT}")"
ZODEX_CODESIGN_IDENTITY=""
uname() { printf 'Darwin\n'; }
file() { printf 'Mach-O 64-bit executable arm64\n'; }
security() { printf '  1) HASH "Zodex Local Development"\n'; }
codesign() {
  if [[ "$1" == "-dv" ]]; then
    printf 'Signature=adhoc\n' >&2
  elif [[ "$1" == "--force" ]]; then
    printf '%s\n' "$*" >>"${TRACE}"
  fi
}
log() { :; }
die() { printf '%s\n' "$*" >&2; exit 1; }
sign_local_operator_if_configured "${BINARY}"
"#;
    let temporary =
        std::env::temp_dir().join(format!("zodex-signing-install-test-{}", std::process::id()));
    std::fs::create_dir_all(&temporary).expect("create signing test directory");
    let trace = temporary.join("trace");
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", install_script_path())
        .env("TRACE", &trace)
        .env("BINARY", temporary.join("zodex"))
        .output()
        .expect("exercise local operator signing");
    assert!(
        output.status.success(),
        "signing fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocation = std::fs::read_to_string(&trace).expect("read codesign invocation");
    assert!(invocation.contains("--sign Zodex Local Development"));
    assert!(invocation.contains("--identifier com.amxv.zodex.local"));
    std::fs::remove_dir_all(&temporary).expect("remove signing test directory");
}

#[test]
fn operator_upgrade_can_install_preextracted_release_without_fresh_install_chatter() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-operator-upgrade-source-test-{}-{unique}",
        std::process::id()
    ));
    let source_dir = test_dir.join("source");
    let install_dir = test_dir.join("install");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    std::fs::create_dir_all(&install_dir).expect("create install directory");
    let source_binary = source_dir.join("zodex");
    std::fs::write(&source_binary, "#!/bin/sh\necho upgraded\n").expect("write source binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&source_binary)
            .expect("read source binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&source_binary, permissions).expect("make source executable");
    }

    let output = Command::new("bash")
        .arg(install_script_path())
        .env("ZODEX_INSTALL_MODE", "operator")
        .env("ZODEX_BINARY_SOURCE_DIR", &source_dir)
        .env("ZODEX_INSTALL_DIR", &install_dir)
        .env("ZODEX_UPGRADE_MODE", "1")
        .env("HOME", test_dir.join("home"))
        .output()
        .expect("run preextracted operator upgrade install");

    assert!(
        output.status.success(),
        "upgrade source install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(install_dir.join("zodex").is_file());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("zodex operator CLI installed"),
        "upgrade mode should not replay fresh-install instructions"
    );

    std::fs::remove_dir_all(&test_dir).expect("remove upgrade source test directory");
}

#[cfg(target_os = "macos")]
#[test]
fn upgrading_a_running_mac_menu_relaunches_the_new_app_without_login_autostart() {
    let root = tempfile::tempdir().expect("temporary installer fixture");
    let source = root.path().join("source");
    let installed = root.path().join("installed");
    let source_app = source.join("Zodex.app/Contents/MacOS/zodex-menubar");
    let installed_app = installed.join("Zodex.app/Contents/MacOS/zodex-menubar");
    std::fs::create_dir_all(source_app.parent().unwrap()).unwrap();
    std::fs::create_dir_all(installed_app.parent().unwrap()).unwrap();
    std::fs::write(source.join("zodex"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(source.join("zodex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&source_app, "new menu binary").unwrap();
    std::fs::write(&installed_app, "old menu binary").unwrap();
    std::fs::set_permissions(&installed_app, std::fs::Permissions::from_mode(0o755)).unwrap();

    let script = r#"
set -euo pipefail
eval "$(sed -n '/^install_operator_binary_atomically()/,/^}/p' "$INSTALL_SCRIPT")"
eval "$(sed -n '/^install_operator_binaries_from_dir()/,/^}/p' "$INSTALL_SCRIPT")"
uname() { printf 'Darwin\n'; }
ensure_local_stopped_before_operator_replace() { :; }
sign_local_operator_if_configured() { :; }
stop_zodex_menubar_before_replace() { printf 'STOP_MENU:%s\n' "$1"; return 0; }
relaunch_zodex_menubar_after_upgrade() { printf 'RELAUNCH_MENU:%s:--after-upgrade\n' "$1"; }
die() { printf '%s\n' "$*" >&2; exit 1; }
warn() { printf 'warning: %s\n' "$*" >&2; }
log() { :; }
install_operator_binaries_from_dir "$SOURCE_DIR"
"#;
    let output = Command::new("bash")
        .args(["-c", script])
        .env("INSTALL_SCRIPT", install_script_path())
        .env("SOURCE_DIR", &source)
        .env("ZODEX_INSTALL_DIR", &installed)
        .env("ZODEX_UPGRADE_MODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout);
    assert!(messages.contains("STOP_MENU:"), "{messages}");
    assert!(messages.contains("RELAUNCH_MENU:"), "{messages}");
    assert!(messages.contains(":--after-upgrade"), "{messages}");
    assert_eq!(std::fs::read(&installed_app).unwrap(), b"new menu binary");
    let installer = std::fs::read_to_string(install_script_path()).unwrap();
    assert!(
        installer.contains("/usr/bin/open -n -g") && installer.contains("--args --after-upgrade"),
        "a new process must be launched, not the stale LaunchServices instance"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn installer_recognizes_a_menu_app_launched_with_after_upgrade_argument() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("Zodex.app/Contents/MacOS/zodex-menubar");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    let c_source = root.path().join("fixture.c");
    std::fs::write(
        &c_source,
        "#include <unistd.h>\nint main(void) { sleep(15); return 0; }\n",
    )
    .unwrap();
    let built = Command::new("/usr/bin/cc")
        .arg(&c_source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "could not build process fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let mut process = Command::new(&binary)
        .arg("--after-upgrade")
        .spawn()
        .unwrap();
    let script = r#"
eval "$(sed -n '/^running_zodex_menubar_pids()/,/^}/p' "$INSTALL_SCRIPT")"
running_zodex_menubar_pids "$MENU_EXE"
"#;
    let output = Command::new("bash")
        .args(["-c", script])
        .env("INSTALL_SCRIPT", install_script_path())
        .env("MENU_EXE", &binary)
        .output()
        .unwrap();
    let _ = process.kill();
    let _ = process.wait();
    assert!(
        output.status.success(),
        "menu process detection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let detected = String::from_utf8_lossy(&output.stdout);
    assert!(
        detected
            .lines()
            .any(|line| line.trim() == process.id().to_string()),
        "installer did not recognize running --after-upgrade menu process: {detected}"
    );
}

#[test]
fn public_install_docs_invoke_the_bash_installer() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "README.md",
        "docs/src/content/docs/local.md",
        "docs/src/content/docs/local/setup.md",
        "docs/src/content/docs/sprite.md",
    ] {
        let text = std::fs::read_to_string(root.join(relative)).expect("read public install docs");
        assert!(
            text.contains("curl -fsSL https://zodex.ashray.xyz/install.sh | bash"),
            "{relative} should invoke the Bash installer"
        );
        assert!(
            !text.contains("curl -fsSL https://zodex.ashray.xyz/install.sh | sh"),
            "{relative} must not invoke a Bash script through generic sh"
        );
    }
}

#[test]
fn operator_install_falls_back_to_user_local_bin_without_root() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-install-test-{}-{unique}",
        std::process::id()
    ));
    let source_dir = test_dir.join("source");
    let home_dir = test_dir.join("home");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    std::fs::create_dir_all(&home_dir).expect("create home directory");

    let source_binary = source_dir.join("zodex");
    let source_menubar = source_dir.join("Zodex.app/Contents/MacOS/zodex-menubar");
    std::fs::create_dir_all(source_menubar.parent().unwrap()).expect("create menu bar app");
    std::fs::write(&source_binary, "#!/bin/sh\nexit 0\n").expect("write source binary");
    std::fs::write(&source_menubar, "#!/bin/sh\nexit 0\n").expect("write menu bar helper");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&source_binary, &source_menubar] {
            let mut permissions = std::fs::metadata(path)
                .expect("read source binary metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).expect("make source binary executable");
        }
    }

    let script_path = install_script_path();
    let command = r#"
eval "$(sed -n '/^operator_local_runtime_dir()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^ensure_local_stopped_before_operator_replace()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^resolve_operator_codesign_identity()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^sign_local_operator_if_configured()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^install_operator_binary_atomically()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^install_operator_binaries_from_dir()/,/^}/p' "${INSTALL_SCRIPT}")"
uname() { printf 'Darwin\n'; }
is_root() { return 1; }
log() { printf '%s\n' "$*"; }
die() { printf '%s\n' "$*" >&2; exit 1; }
install_operator_binaries_from_dir "${SOURCE_DIR}"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", &script_path)
        .env("SOURCE_DIR", &source_dir)
        .env("HOME", &home_dir)
        .env("ZODEX_INSTALL_DIR", "/usr/local/bin")
        .env_remove("ZODEX_UPGRADE_MODE")
        .env_remove("ZODEX_BINARY_SOURCE_DIR")
        .output()
        .expect("run operator install");

    let installed_binary = home_dir.join(".local/bin/zodex");
    let installed_menubar = home_dir.join(".local/bin/Zodex.app/Contents/MacOS/zodex-menubar");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("operator install failed: {stderr}");
    }
    assert!(
        installed_binary.is_file(),
        "operator install should fall back to {}",
        installed_binary.display()
    );
    assert!(
        installed_menubar.is_file(),
        "operator install should include the macOS menu bar helper at {}",
        installed_menubar.display()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("{} --version", installed_binary.display())),
        "installer should print an absolute verification command: {stdout}"
    );
    assert!(stdout.contains("is not currently on PATH"));
    assert!(stdout.contains("export PATH="));

    std::fs::remove_dir_all(&test_dir).expect("remove test directory");
}

#[test]
fn operator_update_refuses_to_replace_zodex_while_local_runtime_state_exists() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-local-update-guard-test-{}-{unique}",
        std::process::id()
    ));
    let source_dir = test_dir.join("source");
    let install_dir = test_dir.join("install");
    let state_home = test_dir.join("state");
    let runtime_dir = state_home.join("zodex/local/runtime");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    std::fs::create_dir_all(&install_dir).expect("create install directory");
    std::fs::create_dir_all(&runtime_dir).expect("create Local runtime directory");

    let source_binary = source_dir.join("zodex");
    let installed_binary = install_dir.join("zodex");
    std::fs::write(&source_binary, "#!/bin/sh\necho new\n").expect("write replacement binary");
    std::fs::write(&installed_binary, "#!/bin/sh\necho old\n").expect("write installed binary");
    std::fs::write(runtime_dir.join("state.json"), "{}\n").expect("write runtime marker");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&source_binary, &installed_binary] {
            let mut permissions = std::fs::metadata(path)
                .expect("read binary metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).expect("make binary executable");
        }
    }

    let script_path = install_script_path();
    let command = r#"
eval "$(sed -n '/^operator_local_runtime_dir()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^ensure_local_stopped_before_operator_replace()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^resolve_operator_codesign_identity()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^sign_local_operator_if_configured()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^install_operator_binary_atomically()/,/^}/p' "${INSTALL_SCRIPT}")"
eval "$(sed -n '/^install_operator_binaries_from_dir()/,/^}/p' "${INSTALL_SCRIPT}")"
uname() { printf 'Darwin\n'; }
is_root() { return 0; }
log() { printf '%s\n' "$*"; }
die() { printf '%s\n' "$*" >&2; exit 1; }
install_operator_binaries_from_dir "${SOURCE_DIR}"
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", &script_path)
        .env("SOURCE_DIR", &source_dir)
        .env("ZODEX_INSTALL_DIR", &install_dir)
        .env("XDG_STATE_HOME", &state_home)
        .env("HOME", test_dir.join("home"))
        .output()
        .expect("run guarded operator update");

    assert!(!output.status.success(), "active Local update must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("run 'zodex local stop' before upgrading"));
    assert_eq!(
        std::fs::read_to_string(&installed_binary).expect("read preserved installed binary"),
        "#!/bin/sh\necho old\n"
    );

    std::fs::remove_dir_all(&test_dir).expect("remove update guard test directory");
}

#[test]
fn atomic_operator_replacement_preserves_existing_binary_if_rename_fails() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-atomic-install-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&test_dir).expect("create atomic install test directory");
    let source = test_dir.join("new-zodex");
    let destination = test_dir.join("zodex");
    std::fs::write(&source, "new\n").expect("write source");
    std::fs::write(&destination, "old\n").expect("write destination");

    let script_path = install_script_path();
    let command = r#"
eval "$(sed -n '/^install_operator_binary_atomically()/,/^}/p' "${INSTALL_SCRIPT}")"
sign_local_operator_if_configured() { :; }
mv() { return 1; }
if install_operator_binary_atomically "${SOURCE}" "${DESTINATION}"; then
  exit 9
fi
"#;
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("INSTALL_SCRIPT", &script_path)
        .env("SOURCE", &source)
        .env("DESTINATION", &destination)
        .output()
        .expect("run atomic replacement failure fixture");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&destination).expect("read preserved destination"),
        "old\n"
    );
    assert_eq!(
        std::fs::read_dir(&test_dir)
            .expect("list atomic install directory")
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".zodex-install."))
            .count(),
        0,
        "failed replacement must clean temporary install files"
    );

    std::fs::remove_dir_all(&test_dir).expect("remove atomic install test directory");
}

#[test]
fn operator_release_dir_install_proves_local_needs_only_zodex_binary() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let test_dir = std::env::temp_dir().join(format!(
        "zodex-local-install-test-{}-{unique}",
        std::process::id()
    ));
    let release_dir = test_dir.join("zodex-test-release");
    let install_dir = test_dir.join("install");
    std::fs::create_dir_all(&release_dir).expect("create release directory");
    let real_zodex = env!("CARGO_BIN_EXE_zodex");
    assert!(
        !real_zodex.contains('\''),
        "test binary path unexpectedly contains a shell quote"
    );
    let fixture_zodex = release_dir.join("zodex");
    std::fs::write(
        &fixture_zodex,
        format!("#!/bin/sh\nexec '{real_zodex}' \"$@\"\n"),
    )
    .expect("write thin operator release fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&fixture_zodex)
            .expect("read fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fixture_zodex, permissions).expect("make fixture executable");
    }

    let archive = test_dir.join("zodex-test-release.tar.gz");
    let tar_output = Command::new("tar")
        .args(["-C", test_dir.to_str().expect("utf-8 test path"), "-czf"])
        .arg(&archive)
        .arg(release_dir.file_name().expect("release directory name"))
        .output()
        .expect("create operator release archive");
    assert!(
        tar_output.status.success(),
        "tar failed: {}",
        String::from_utf8_lossy(&tar_output.stderr)
    );

    let archive_bytes = std::fs::read(&archive).expect("read operator release archive");
    let digest = Sha256::digest(&archive_bytes);
    std::fs::write(
        format!("{}.sha256", archive.display()),
        format!(
            "{digest:x}  {}\n",
            archive.file_name().unwrap().to_string_lossy()
        ),
    )
    .expect("write operator release checksum");
    let asset_url = format!("file://{}", archive.display());

    let output = Command::new("bash")
        .arg(install_script_path())
        .env("ZODEX_ASSET_URL", &asset_url)
        .env("ZODEX_INSTALL_DIR", &install_dir)
        .env_remove("ZODEX_UPGRADE_MODE")
        .env_remove("ZODEX_BINARY_SOURCE_DIR")
        .output()
        .expect("run operator release-dir install");
    if !output.status.success() {
        panic!(
            "operator install failed\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed = install_dir.join("zodex");
    assert!(installed.is_file());
    for absent in ["zodex-client", "zodex-agent", "zodexd", "zodex-prd"] {
        assert!(!install_dir.join(absent).exists(), "unexpected {absent}");
    }

    let help = Command::new(&installed)
        .args(["local", "--help"])
        .output()
        .expect("run installed Local help");
    assert!(help.status.success());

    let setup_help = Command::new(&installed)
        .args(["local", "setup", "--help"])
        .output()
        .expect("run installed Local setup help");
    assert!(setup_help.status.success());

    std::fs::remove_dir_all(&test_dir).expect("remove Local install test directory");
}
