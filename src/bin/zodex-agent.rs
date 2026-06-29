use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use zodex::config::DEFAULT_CONFIG_PATH;

#[derive(Debug, Parser)]
#[command(name = "zodex-agent")]
#[command(about = "Restricted Zodex agent CLI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    GitCredentialHelper {
        operation: String,
    },
    ShowUrl {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
}

#[derive(Debug, Subcommand)]
enum GithubCommand {
    RequestPush {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        publisher_client_id: Option<String>,
        #[arg(long, default_value = "30m")]
        ttl: String,
        #[arg(long, default_value_t = false)]
        no_ttl: bool,
        #[arg(long, default_value_t = false)]
        cache_refresh_token: bool,
    },
    RevokePush {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value_t = false)]
        forget_local_auth: bool,
    },
    ListGrants,
    PublishPr {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "main")]
        base: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, default_value_t = false)]
        draft: bool,
    },
}

fn resolve_runtime_binary() -> Result<PathBuf> {
    let current_exe = env::current_exe().context("failed to resolve current executable path")?;
    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join("zodexd");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    let fallback = PathBuf::from("/usr/local/bin/zodexd");
    if fallback.is_file() {
        return Ok(fallback);
    }

    if command_exists("zodexd") {
        return Ok(PathBuf::from("zodexd"));
    }

    bail!(
        "failed to locate the guest runtime helper; expected a sibling `zodexd` binary or `/usr/local/bin/zodexd`"
    );
}

fn command_exists(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| Path::new(&dir).join(program).is_file())
}

fn build_runtime_args(cli: Cli) -> Vec<String> {
    let mut args = vec!["--config".to_string(), cli.config];
    match cli.command {
        Commands::GitCredentialHelper { operation } => {
            args.push("git-credential-helper".to_string());
            args.push(operation);
        }
        Commands::ShowUrl { host } => {
            args.push("show-url".to_string());
            args.push("--host".to_string());
            args.push(host);
        }
        Commands::Github { command } => {
            args.push("github".to_string());
            match command {
                GithubCommand::RequestPush {
                    repo,
                    publisher_client_id,
                    ttl,
                    no_ttl,
                    cache_refresh_token,
                } => {
                    args.push("request-push".to_string());
                    args.push("--repo".to_string());
                    args.push(repo);
                    if let Some(publisher_client_id) = publisher_client_id {
                        args.push("--publisher-client-id".to_string());
                        args.push(publisher_client_id);
                    }
                    args.push("--ttl".to_string());
                    args.push(ttl);
                    if no_ttl {
                        args.push("--no-ttl".to_string());
                    }
                    if cache_refresh_token {
                        args.push("--cache-refresh-token".to_string());
                    }
                }
                GithubCommand::RevokePush {
                    repo,
                    forget_local_auth,
                } => {
                    args.push("revoke-push".to_string());
                    args.push("--repo".to_string());
                    args.push(repo);
                    if forget_local_auth {
                        args.push("--forget-local-auth".to_string());
                    }
                }
                GithubCommand::ListGrants => {
                    args.push("list-grants".to_string());
                }
                GithubCommand::PublishPr {
                    repo,
                    title,
                    base,
                    body,
                    draft,
                } => {
                    args.push("publish-pr".to_string());
                    args.push("--repo".to_string());
                    args.push(repo);
                    args.push("--title".to_string());
                    args.push(title);
                    args.push("--base".to_string());
                    args.push(base);
                    args.push("--body".to_string());
                    args.push(body);
                    if draft {
                        args.push("--draft".to_string());
                    }
                }
            }
        }
    }
    args
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let runtime = resolve_runtime_binary()?;
    let status = Command::new(&runtime)
        .args(build_runtime_args(cli))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute {}", runtime.display()))?;

    if let Some(code) = status.code() {
        return Ok(ExitCode::from(code as u8));
    }

    Err(anyhow!(
        "{} terminated without an exit code",
        runtime.display()
    ))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
