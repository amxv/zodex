use std::path::Path;

use anyhow::Result;
use clap::Parser;
use tracing::warn;
use zodex::config::{Config, DEFAULT_CONFIG_PATH};
use zodex::install_rustls_crypto_provider;
use zodex::server::run_server;

#[derive(Debug, Parser)]
#[command(name = "zodexd")]
#[command(about = "Zodex daemon for remote execution")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "zodex=info,zodexd=info".to_string()),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(Some(Path::new(&args.config)))?;

    warn!("zodexd exposes high-privilege remote execution; protect API keys and network access");

    run_server(config).await
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::CommandFactory;

    #[test]
    fn clap_help_uses_zodexd_name() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("zodexd"));
        assert!(help.contains("remote execution"));
    }
}
