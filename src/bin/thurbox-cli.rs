//! Thurbox CLI binary — scriptable access to the same state exposed by the
//! MCP server and the TUI. Every subcommand works without the TUI running.

use clap::Parser;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = thurbox::cli::Cli::parse();

    let db_path = match thurbox::paths::database_file() {
        Some(p) => p,
        None => {
            eprintln!("error: cannot resolve database path (is HOME set?)");
            std::process::exit(2);
        }
    };

    let db = match thurbox::storage::Database::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "error: failed to open database at {}: {e}",
                db_path.display()
            );
            std::process::exit(2);
        }
    };

    if let Err(e) = thurbox::cli::run(cli, &db) {
        let msg = serde_json::json!({ "error": e }).to_string();
        eprintln!("{msg}");
        std::process::exit(1);
    }
}
