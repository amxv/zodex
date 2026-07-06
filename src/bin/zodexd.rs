use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::warn;
use zodex::config::{Config, DEFAULT_CONFIG_PATH};
use zodex::install_rustls_crypto_provider;
use zodex::redaction::redact_api_key_query_params;
use zodex::server::run_server;

#[path = "zodexd/git_remote.rs"]
mod git_remote;
#[path = "zodexd/github.rs"]
mod github;
#[path = "zodexd/tls.rs"]
mod tls;

use git_remote::{handle_git_credential_helper, handle_git_remote_zodex};
use github::{
    DEFAULT_PUSH_GRANT_TTL_SECONDS, list_push_grants, parse_push_grant_ttl, publish_pr,
    request_push_access, revoke_push_access,
};
use tls::ensure_tls_artifacts;

#[cfg(test)]
use git_remote::{
    create_direct_push_bundle_base64_from_dir, git_remote_zodex_push_dst, git_remote_zodex_repo,
    resolve_git_object_id, resolve_git_object_type, sanitize_remote_helper_error,
};
#[cfg(test)]
use github::{PushGrantRecord, parse_push_grants, resolve_active_push_grant};

#[derive(Debug, Parser)]
#[command(name = "zodexd")]
#[command(about = "Zodex daemon for remote execution")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(hide = true)]
    GitCredentialHelper { operation: String },
    #[command(hide = true)]
    GitRemoteZodex { remote: String, url: String },
    #[command(hide = true)]
    ShowUrl {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    #[command(hide = true)]
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
    #[command(hide = true)]
    EnsureTls,
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
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, default_value_t = false)]
        draft: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    let args = Args::parse();
    match args.command {
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    std::env::var("RUST_LOG")
                        .unwrap_or_else(|_| "zodex=info,zodexd=info".to_string()),
                )
                .init();

            let config = Config::load(Some(Path::new(&args.config)))?;
            warn!(
                "zodexd exposes high-privilege remote execution; protect API keys and network access"
            );
            run_server(config).await
        }
        Some(command) => run_hidden_command(Path::new(&args.config), command).await,
    }
}

async fn run_hidden_command(config_path: &Path, command: Commands) -> Result<()> {
    match command {
        Commands::GitCredentialHelper { operation } => {
            let config = Config::load(Some(config_path))?;
            handle_git_credential_helper(&config, &operation).await?;
        }
        Commands::GitRemoteZodex { remote: _, url } => {
            let config = Config::load(Some(config_path))?;
            handle_git_remote_zodex(&config, &url).await?;
        }
        Commands::ShowUrl { host } => {
            let config = Config::load(Some(config_path))?;
            let raw_url = format!("https://{host}/mcp?key={}", config.api_key);
            println!(
                "{} (key redacted in CLI output)",
                redact_api_key_query_params(&raw_url)
            );
        }
        Commands::Github { command } => {
            let config = Config::load(Some(config_path))?;
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
                GithubCommand::RevokePush {
                    repo,
                    forget_local_auth,
                } => {
                    revoke_push_access(&repo, forget_local_auth)?;
                }
                GithubCommand::ListGrants => {
                    list_push_grants()?;
                }
                GithubCommand::PublishPr {
                    repo,
                    title,
                    base,
                    body,
                    draft,
                } => {
                    publish_pr(&config, &repo, &title, base.as_deref(), &body, draft).await?;
                }
            }
        }
        Commands::EnsureTls => {
            ensure_tls_artifacts(config_path)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::{
        Args, PushGrantRecord, create_direct_push_bundle_base64_from_dir,
        git_remote_zodex_push_dst, git_remote_zodex_repo, parse_push_grants,
        resolve_active_push_grant, resolve_git_object_id, resolve_git_object_type,
        sanitize_remote_helper_error,
    };
    use clap::CommandFactory;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git_test_status(command: &mut Command) -> bool {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git")
            .success()
    }

    #[test]
    fn clap_help_uses_zodexd_name() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("zodexd"));
        assert!(help.contains("remote execution"));
        assert!(!help.contains("git-credential-helper"));
        assert!(!help.contains("ensure-tls"));
    }

    #[test]
    fn push_grant_resolver_requires_an_active_push_grant() {
        let grants_dir = tempdir().expect("grants dir");
        let err = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect_err("missing grant should error");
        let message = err.to_string();
        assert!(message.contains("no active push grant"));
        assert!(message.contains("request-push"));
    }

    #[test]
    fn push_grant_resolver_reuses_the_active_push_grant_token() {
        let grants_dir = tempdir().expect("grants dir");
        let grant = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_example_token".to_string(),
            expires_at: None,
            expires_at_epoch_seconds: None,
            token_source: Some("github-app-user-token".to_string()),
        };
        fs::write(
            grants_dir.path().join("owner__repo.json"),
            serde_json::to_vec(&grant).expect("encode grant"),
        )
        .expect("write grant");

        let resolved = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect("grant should resolve");
        assert_eq!(resolved.token, "ghu_example_token");
    }

    #[test]
    fn zodex_remote_helper_extracts_github_repo_without_credentials() {
        assert_eq!(
            git_remote_zodex_repo("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            git_remote_zodex_repo("https://token@github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            git_remote_zodex_repo("zodex::https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn zodex_remote_helper_parses_push_destination_and_sanitizes_errors() {
        assert_eq!(
            git_remote_zodex_push_dst("+refs/heads/main:refs/heads/main"),
            Some("refs/heads/main".to_string())
        );
        assert_eq!(
            sanitize_remote_helper_error(&anyhow::anyhow!("first line\nsecond line")),
            "first line second line"
        );
    }

    #[test]
    fn direct_push_bundle_imports_into_clone_with_remote_prerequisites() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");
        let publisher_clone = tempdir.path().join("publisher-clone");
        let bundle_path = tempdir.path().join("direct-push.bundle");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["checkout", "-q", "-b", "smoke"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "smoke"
            ])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/heads/smoke")
            .expect("create bundle")
            .expect("branch push should have bundle contents");
        fs::write(
            &bundle_path,
            base64::engine::general_purpose::STANDARD
                .decode(bundle_base64)
                .expect("decode bundle"),
        )
        .expect("write bundle");

        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            publisher_clone.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git").current_dir(&publisher_clone).args([
                "fetch",
                bundle_path.to_str().unwrap(),
                "refs/heads/smoke:refs/zodex/direct-push",
            ])
        ));
    }

    #[test]
    fn direct_push_bundle_imports_annotated_tag_without_heads_namespace() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");
        let publisher_clone = tempdir.path().join("publisher-clone");
        let bundle_path = tempdir.path().join("direct-push.bundle");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "fetch",
                "-q",
                "origin",
                "main:refs/remotes/origin/main"
            ])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["tag", "-a", "v1.0.0", "-m", "v1.0.0"])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/tags/v1.0.0")
            .expect("create tag bundle")
            .expect(
                "annotated tag object should produce a bundle even when target commit is remote",
            );
        fs::write(
            &bundle_path,
            base64::engine::general_purpose::STANDARD
                .decode(bundle_base64)
                .expect("decode bundle"),
        )
        .expect("write bundle");

        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            publisher_clone.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git").current_dir(&publisher_clone).args([
                "fetch",
                bundle_path.to_str().unwrap(),
                "refs/tags/v1.0.0:refs/zodex/direct-push",
            ])
        ));
        let object_type = resolve_git_object_type(&publisher_clone, "refs/zodex/direct-push")
            .expect("imported ref object type");
        assert_eq!(object_type, "tag");
    }

    #[test]
    fn direct_push_bundle_uses_oid_fallback_for_lightweight_tag_on_remote_commit() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "fetch",
                "-q",
                "origin",
                "main:refs/remotes/origin/main"
            ])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["tag", "v1.0.0"])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/tags/v1.0.0")
            .expect("lightweight tag bundle attempt should not hard fail");
        assert!(
            bundle_base64.is_none(),
            "lightweight tag on an already-remote commit has no new bundle objects"
        );
        let tag_oid = resolve_git_object_id(&repo, "refs/tags/v1.0.0").expect("tag oid");
        let head_oid = resolve_git_object_id(&repo, "HEAD").expect("head oid");
        assert_eq!(tag_oid, head_oid);
        assert_eq!(
            resolve_git_object_type(&repo, "refs/tags/v1.0.0").expect("tag object type"),
            "commit"
        );
    }

    #[test]
    fn push_grant_resolver_rejects_expired_push_grant() {
        let grants_dir = tempdir().expect("grants dir");
        let grant = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_expired".to_string(),
            expires_at: Some("1970-01-01T00:00:01Z".to_string()),
            expires_at_epoch_seconds: Some(1),
            token_source: Some("github-app-user-token".to_string()),
        };
        let path = grants_dir.path().join("owner__repo.json");
        fs::write(&path, serde_json::to_vec(&grant).expect("encode grant")).expect("write grant");

        let err = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect_err("expired grant should error");
        assert!(err.to_string().contains("no active push grant"));
        assert!(!path.exists(), "expired grant file should be pruned");
    }

    #[test]
    fn parse_push_grants_accepts_pretty_printed_grant_stream() {
        let first = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_first".to_string(),
            expires_at: Some("2026-06-30T00:00:00Z".to_string()),
            expires_at_epoch_seconds: Some(1_782_777_600),
            token_source: Some("github-app-user-token".to_string()),
        };
        let second = PushGrantRecord {
            repo: "owner/other".to_string(),
            token: "ghu_second".to_string(),
            expires_at: None,
            expires_at_epoch_seconds: None,
            token_source: Some("github-app-user-token".to_string()),
        };
        let raw = format!(
            "{}\n{}\n",
            serde_json::to_string_pretty(&first).expect("encode first grant"),
            serde_json::to_string_pretty(&second).expect("encode second grant")
        );

        let grants = parse_push_grants(&raw).expect("pretty grant stream should parse");

        assert_eq!(grants, vec![first, second]);
    }
}
