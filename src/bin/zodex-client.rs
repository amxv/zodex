use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use zodex::client::{
    ConnectionProfile, ZodexClient, delete_profile, resolve_connect_connection,
    resolve_operation_connection, save_profile,
};
use zodex::install_rustls_crypto_provider;
use zodex::protocol::{ApplyPatchInput, ExecCommandInput, WriteStdinInput};

#[derive(Debug, Parser)]
#[command(name = "zodex-client")]
#[command(about = "Thin HTTP CLI for remote zodex operations")]
#[command(version)]
struct Cli {
    #[arg(long, global = true)]
    url: Option<String>,

    #[arg(long, global = true)]
    key: Option<String>,

    #[arg(long, global = true, hide = true)]
    profile_path: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Connect,
    Disconnect,
    ExecCommand {
        #[arg(value_name = "CMD")]
        cmd: String,
        #[arg(long)]
        yield_time_ms: Option<u64>,
        #[arg(long)]
        workdir: Option<String>,
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    WriteStdin {
        #[arg(long)]
        session_handle: String,
        #[arg(long)]
        chars: Option<String>,
        #[arg(long)]
        yield_time_ms: Option<u64>,
        #[arg(long)]
        kill_process: bool,
    },
    ApplyPatch {
        #[arg(long)]
        patch: String,
        #[arg(long)]
        workdir: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    let cli = Cli::parse();
    let profile_path = cli.profile_path.as_deref();

    match cli.command {
        Commands::Connect => {
            let saved = run_connect(cli.url, cli.key, profile_path).await?;
            println!("saved connection profile at {}", saved.display());
        }
        Commands::Disconnect => {
            let removed = run_disconnect(profile_path)?;
            if removed {
                println!("removed saved connection profile");
            } else {
                println!("no saved connection profile found");
            }
        }
        Commands::ExecCommand {
            cmd,
            yield_time_ms,
            workdir,
            timeout_ms,
        } => {
            let client = resolved_client(cli.url, cli.key, profile_path)?;
            let output = client
                .exec_command(ExecCommandInput {
                    cmd,
                    yield_time_ms,
                    workdir,
                    timeout_ms,
                })
                .await?;
            print_json(&output)?;
        }
        Commands::WriteStdin {
            session_handle,
            chars,
            yield_time_ms,
            kill_process,
        } => {
            let client = resolved_client(cli.url, cli.key, profile_path)?;
            let output = client
                .write_stdin(WriteStdinInput {
                    session_handle,
                    chars,
                    yield_time_ms,
                    kill_process: Some(kill_process),
                })
                .await?;
            print_json(&output)?;
        }
        Commands::ApplyPatch { patch, workdir } => {
            let client = resolved_client(cli.url, cli.key, profile_path)?;
            let output = client
                .apply_patch(ApplyPatchInput { patch, workdir })
                .await?;
            print_json(&output)?;
        }
    }

    Ok(())
}

async fn run_connect(
    url: Option<String>,
    key: Option<String>,
    profile_path: Option<&Path>,
) -> Result<PathBuf> {
    let resolved = resolve_connect_connection(url, key)?;
    let client = ZodexClient::new(resolved.url.clone(), resolved.key.clone());
    client.verify_connection().await?;
    save_profile(
        &ConnectionProfile {
            url: resolved.url,
            key: resolved.key,
        },
        profile_path,
    )
}

fn run_disconnect(profile_path: Option<&Path>) -> Result<bool> {
    delete_profile(profile_path)
}

fn resolved_client(
    url: Option<String>,
    key: Option<String>,
    profile_path: Option<&Path>,
) -> Result<ZodexClient> {
    let resolved = resolve_operation_connection(url, key, profile_path)?;
    Ok(ZodexClient::new(resolved.url, resolved.key))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::serve;
    use clap::{CommandFactory, Parser};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::{Cli, Commands, run_connect, run_disconnect};
    use zodex::config::Config;
    use zodex::http_api::build_http_api_router;
    use zodex::service::ZodexService;

    #[test]
    fn clap_help_uses_zodex_client_name() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("remote zodex operations"));
        assert!(help.contains("zodex-client"));
    }

    #[test]
    fn parses_exec_command_and_global_connection_flags() {
        let cli = Cli::try_parse_from([
            "zodex-client",
            "--url",
            "https://example.invalid",
            "--key",
            "abc",
            "exec-command",
            "echo hi",
            "--yield-time-ms",
            "123",
            "--workdir",
            "/tmp",
            "--timeout-ms",
            "999",
        ])
        .expect("cli should parse");

        assert_eq!(cli.url.as_deref(), Some("https://example.invalid"));
        assert_eq!(cli.key.as_deref(), Some("abc"));
        match cli.command {
            Commands::ExecCommand {
                cmd,
                yield_time_ms,
                workdir,
                timeout_ms,
            } => {
                assert_eq!(cmd, "echo hi");
                assert_eq!(yield_time_ms, Some(123));
                assert_eq!(workdir.as_deref(), Some("/tmp"));
                assert_eq!(timeout_ms, Some(999));
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    #[test]
    fn parses_write_stdin_command_options() {
        let cli = Cli::try_parse_from([
            "zodex-client",
            "write-stdin",
            "--session-handle",
            "handle-42",
            "--chars",
            "echo hi\n",
            "--yield-time-ms",
            "200",
            "--kill-process",
        ])
        .expect("cli should parse");

        match cli.command {
            Commands::WriteStdin {
                session_handle,
                chars,
                yield_time_ms,
                kill_process,
            } => {
                assert_eq!(session_handle, "handle-42");
                assert_eq!(chars.as_deref(), Some("echo hi\n"));
                assert_eq!(yield_time_ms, Some(200));
                assert!(kill_process);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }
    }

    async fn spawn_test_http_api(
        api_key: &str,
    ) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let config = Arc::new(Config {
            api_key: api_key.to_string(),
            ..Config::default()
        });
        let service = ZodexService::new(config.clone());
        let app = build_http_api_router(config, service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server should run");
        });

        (format!("http://{addr}"), shutdown_tx, server)
    }

    #[tokio::test]
    async fn connect_and_disconnect_persist_profile_at_overridden_path() {
        zodex::install_rustls_crypto_provider();

        let dir = tempdir().expect("tempdir");
        let profile_path = dir.path().join("profile.json");
        let api_key = "abc123";
        let (url, shutdown_tx, server) = spawn_test_http_api(api_key).await;

        let saved = run_connect(
            Some(url),
            Some(api_key.to_string()),
            Some(profile_path.as_path()),
        )
        .await
        .expect("connect should verify and save profile");
        assert_eq!(saved, profile_path);
        assert!(saved.exists());

        let removed = run_disconnect(Some(saved.as_path())).expect("disconnect should succeed");
        assert!(removed);
        assert!(!saved.exists());

        let _ = shutdown_tx.send(());
        server.await.expect("server join should succeed");
    }

    #[tokio::test]
    async fn connect_fails_without_saving_profile_when_auth_is_invalid() {
        zodex::install_rustls_crypto_provider();

        let dir = tempdir().expect("tempdir");
        let profile_path = dir.path().join("profile.json");
        let (url, shutdown_tx, server) = spawn_test_http_api("expected-key").await;

        let err = run_connect(
            Some(url),
            Some("wrong-key".to_string()),
            Some(profile_path.as_path()),
        )
        .await
        .expect_err("connect should fail for invalid key");
        assert!(
            err.to_string().contains("status 401"),
            "unexpected error: {err:?}"
        );
        assert!(!profile_path.exists());

        let _ = shutdown_tx.send(());
        server.await.expect("server join should succeed");
    }
}
