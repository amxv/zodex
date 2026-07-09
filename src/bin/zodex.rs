#[path = "zodex/mod.rs"]
mod operator_cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    operator_cli::run().await
}
