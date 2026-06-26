use std::path::Path;

use anyhow::Result;
use clap::Parser;
use computer_mcp::config::{Config, DEFAULT_CONFIG_PATH};
use computer_mcp::install_rustls_crypto_provider;
use computer_mcp::publisher::serve_publisher;

#[derive(Debug, Parser)]
#[command(name = "computer-mcp-prd")]
#[command(about = "Legacy internal publisher daemon kept for compatibility during migration")]
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
                .unwrap_or_else(|_| "computer_mcp=info,computer_mcp_prd=info".to_string()),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(Some(Path::new(&args.config)))?;
    serve_publisher(config).await
}
