mod server;
mod session_wrapper;

use anyhow::Result;
use aptos_crypto::HashValue;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "movelite", about = "Lightweight Move VM for local development")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the movelite server
    Start {
        /// Port to listen on
        #[arg(short, long, default_value = "8090")]
        port: u16,

        /// Fork from a remote network URL
        #[arg(long)]
        fork_url: Option<String>,

        /// Fork from a specific remote ledger version
        #[arg(long)]
        fork_version: Option<u64>,

        /// Override the chain id reported to clients and written to local genesis state
        #[arg(long)]
        chain_id: Option<u8>,

        /// Persist session state in this directory instead of using a per-process tempdir
        #[arg(long)]
        session_dir: Option<PathBuf>,

        /// Delete the selected session directory before starting
        #[arg(long)]
        reset: bool,

        /// Token required for movelite-only mutating endpoints such as /mint
        #[arg(long)]
        auth_token: Option<String>,

        /// Disable local auth checks
        #[arg(long)]
        no_auth: bool,

        /// Require the movelite auth token for Aptos-compatible mutating endpoints too
        #[arg(long)]
        strict_local_auth: bool,
    },
    /// Show version
    Version,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            port,
            fork_url,
            fork_version,
            chain_id,
            session_dir,
            reset,
            auth_token,
            no_auth,
            strict_local_auth,
        } => {
            eprintln!("movelite v{}", env!("CARGO_PKG_VERSION"));
            eprintln!();

            let session = session_wrapper::create_session(session_wrapper::SessionOptions {
                fork_url,
                fork_version,
                chain_id,
                session_dir,
                reset,
            })?;

            let token = if no_auth {
                None
            } else {
                Some(auth_token.unwrap_or_else(generate_auth_token))
            };

            eprintln!("Starting server on http://127.0.0.1:{}...", port);
            if let Some(token) = &token {
                eprintln!("movelite auth token: {}", token);
                eprintln!("Use header: x-movelite-token: {}", token);
            } else {
                eprintln!("Local auth disabled.");
            }
            eprintln!("Press Ctrl+C to stop.");
            eprintln!();

            server::run(
                session,
                port,
                server::ServerOptions {
                    auth_token: token,
                    strict_local_auth,
                },
            )
            .await
        }
        Commands::Version => {
            println!("movelite v{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn generate_auth_token() -> String {
    hex::encode(HashValue::random().to_vec())
}
