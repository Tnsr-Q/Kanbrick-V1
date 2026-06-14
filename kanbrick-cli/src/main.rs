//! # kanbrick-cli
//!
//! Admin and data-loading CLI. Phase 0 scaffold: the command surface is
//! declared, and `seed` is a stub that is implemented in Phase 1.

use clap::{Parser, Subcommand};

/// Kanbrick-V1 administrative CLI.
#[derive(Parser)]
#[command(name = "kanbrick-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load seed data into the firm graph (implemented in Phase 1).
    Seed {
        /// Path to a Cypher seed file.
        #[arg(long, default_value = "seed/kanbrick_seed_data.cypher")]
        file: String,
        /// Path to the graph database.
        #[arg(long, default_value = "graph/firm.db")]
        db: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Seed { file, db } => {
            println!("kanbrick-cli seed — scaffold (file: {file}, db: {db})");
            println!("seeding is implemented in Phase 1");
        }
    }
}
