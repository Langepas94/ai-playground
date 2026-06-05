use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    ai_playground::cli::run().await
}
