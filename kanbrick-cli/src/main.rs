//! # kanbrick-cli
//!
//! Admin and data-loading CLI. The `seed` subcommand applies the firm schema
//! and loads seed data into a SparrowDB database (issues #10, #11).

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{Migrator, Store};

/// Kanbrick-V1 administrative CLI.
#[derive(Parser)]
#[command(name = "kanbrick-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply the firm schema and load seed data into the graph.
    ///
    /// Runs the versioned migrations (initial schema, then seed data). Safe to
    /// run repeatedly: already-applied versions are skipped.
    Seed {
        /// Path to a Cypher seed file.
        #[arg(long, default_value = "seed/kanbrick_seed_data.cypher")]
        file: String,
        /// Path to the graph database directory.
        #[arg(long, default_value = "graph/firm.db")]
        db: String,
    },

    /// Set (or reset) a person's login password by email.
    ///
    /// Hashes the password with Argon2id and stores it on the `Person` node so
    /// the account can authenticate via `POST /login`.
    SetPassword {
        /// The person's email (their login handle).
        #[arg(long)]
        email: String,
        /// The plaintext password to set.
        #[arg(long)]
        password: String,
        /// Path to the graph database directory.
        #[arg(long, default_value = "graph/firm.db")]
        db: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Seed { file, db } => run_seed(&file, &db),
        Command::SetPassword {
            email,
            password,
            db,
        } => run_set_password(&email, &password, &db),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Open the database and set `email`'s password.
fn run_set_password(email: &str, password: &str, db: &str) -> kanbrick_core::Result<()> {
    let store = Store::open(db)?;
    // The JWT authenticator is unused for setting a password (no token is issued),
    // but `LoginService` is constructed with one.
    let jwt = JwtAuthenticator::new(b"cli-set-password-unused", chrono::Duration::hours(1));
    LoginService::new(&store, &jwt).set_password(email, password)?;
    store.checkpoint()?;
    println!("set password for {email}");
    Ok(())
}

/// Open the database, then apply schema + seed migrations from `file`.
fn run_seed(file: &str, db: &str) -> kanbrick_core::Result<()> {
    use kanbrick_core::Error;

    let source = std::fs::read_to_string(file)
        .map_err(|e| Error::InvalidInput(format!("cannot read seed file {file}: {e}")))?;

    println!("opening database at {db}");
    let store = Store::open(db)?;

    println!("applying migrations (schema + seed from {file})");
    let outcome = Migrator::firm(source).run(&store)?;

    store.checkpoint()?;

    if outcome.applied.is_empty() {
        println!(
            "nothing to do — migrations {:?} already applied",
            outcome.skipped
        );
    } else {
        println!("applied migrations: {:?}", outcome.applied);
        if !outcome.skipped.is_empty() {
            println!("skipped (already applied): {:?}", outcome.skipped);
        }
    }
    Ok(())
}
