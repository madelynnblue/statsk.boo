//! Thin wrapper for the one-off legacy cache re-key.
//!
//! The production image ships only the main `statskboo` binary, so the required
//! entry point is the `statskboo rekey-cache` subcommand; this bin exists for
//! convenience during development (`cargo run --bin rekey_game_cache -- ...`).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    statskboo::rekey_cache::run_cli(std::env::args().skip(1)).await
}
