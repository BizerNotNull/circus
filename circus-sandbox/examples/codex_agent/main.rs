//! One Codex turn through a bounded model channel, with Docker networking disabled.

mod runner;

use runner::{Config, Result};

#[tokio::main]
async fn main() -> Result {
    let config = Config::from_env()?;
    let artifact = runner::run(config, &format!("{}\nmain_host()\n", runner::BRIDGE)).await?;
    println!(
        "task completed; artifact retrieved ({} bytes)",
        artifact.len()
    );
    // The example's result is data, never interpreted as a local command.
    use std::io::Write;
    std::io::stdout().lock().write_all(&artifact)?;
    Ok(())
}
