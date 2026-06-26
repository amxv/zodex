use std::path::Path;

use anyhow::Result;
use clap::Parser;
use computer_mcp::config::{Config, DEFAULT_CONFIG_PATH};
use computer_mcp::install_rustls_crypto_provider;
use computer_mcp::server::run_server;
use tracing::warn;

#[derive(Debug, Parser)]
#[command(name = "zodexd")]
#[command(about = "Zodex daemon for remote execution (compatible with legacy computer-mcpd)")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "computer_mcp=info,computer_mcpd=info".to_string()),
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
        assert!(help.contains("legacy computer-mcpd"));
    }
}
