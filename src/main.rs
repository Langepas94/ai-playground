use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    aiteach::cli::run().await
}
