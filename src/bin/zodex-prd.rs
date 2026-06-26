use std::path::Path;

use anyhow::Result;
use clap::Parser;
use zodex::config::{Config, DEFAULT_CONFIG_PATH};
use zodex::install_rustls_crypto_provider;
use zodex::publisher::serve_publisher;

#[derive(Debug, Parser)]
#[command(name = "zodex-prd")]
#[command(about = "Internal push-grant support daemon")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "zodex=info,zodex_prd=info".to_string()),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(Some(Path::new(&args.config)))?;
    serve_publisher(config).await
}
