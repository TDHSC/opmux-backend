//! Operator CLI for tenant and API-key provisioning.
//!
//! Successful commands print one JSON object to stdout, including the newly
//! issued credential exactly once. Write that stdout to a fresh private file
//! created with `mktemp` (mode 0600 even under umask 022). Do not redirect
//! onto an existing path or symlink. There is no later secret retrieval;
//! recover by issuing a replacement key. HTTP bootstrap is not provided.

use clap::{Parser, Subcommand, ValueEnum};
use gateway::core::db::DatabasePoolConfig;
use gateway::features::auth::{
    ApiKeyKind, PostgresAuthStore, ProvisionError, ProvisioningService,
};
use std::sync::Arc;
use uuid::Uuid;

const LONG_ABOUT: &str = "\
Operator-only CLI for local and migration environments.

Successful tenant create and key issue commands print one JSON object to
stdout. That object includes the newly generated credential exactly once.
Write it to a fresh private file created with mktemp, then chmod 600. That
file is mode 0600 even when the process umask is 022. Do not redirect onto
an existing path or symlink.

  keyfile=$(mktemp \"${TMPDIR:-/tmp}/opmux-key.XXXXXX\")
  chmod 600 \"$keyfile\"
  opmux-admin tenant create --name acme > \"$keyfile\"

Do not paste secrets into tickets, logs, or shell history. The secret cannot
be retrieved later; issue a replacement key if it is lost.

The CLI is not an unauthenticated HTTP bootstrap. It uses DATABASE_URL and
assumes the connecting user can SET ROLE to OPMUX_DB_ROLE (default
opmux_operator). Schema migrations stay with the database owner via
scripts/db-migrate.sh. The runtime role opmux_runtime cannot create tenants.";

#[derive(Parser, Debug)]
#[command(
    name = "opmux-admin",
    about = "Provision tenants and API keys against local Supabase",
    long_about = LONG_ABOUT,
    after_help = "Secure output: secrets appear only in successful stdout JSON. \
Create a fresh mktemp file (mode 0600) instead of overwriting an existing path \
or symlink. Failed commands print no credential."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a tenant and issue its initial management key
    Tenant {
        #[command(subcommand)]
        command: TenantCommands,
    },
    /// Issue a key for an existing tenant
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },
}

#[derive(Subcommand, Debug)]
enum TenantCommands {
    /// Insert one client and one management key atomically
    Create {
        /// Tenant display name (1–128 characters)
        #[arg(long)]
        name: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "lower")]
enum KeyKindArg {
    Management,
    Inference,
}

impl From<KeyKindArg> for ApiKeyKind {
    fn from(value: KeyKindArg) -> Self {
        match value {
            KeyKindArg::Management => Self::Management,
            KeyKindArg::Inference => Self::Inference,
        }
    }
}

#[derive(Subcommand, Debug)]
enum KeyCommands {
    /// Issue a management or inference key for an existing client
    Issue {
        /// Existing tenant UUID
        #[arg(long, value_name = "UUID")]
        client_id: Uuid,
        /// Immutable key kind
        #[arg(long, value_enum)]
        kind: KeyKindArg,
        /// Key name (1–128 characters)
        #[arg(long)]
        name: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(code) = run(cli).await {
        std::process::exit(code);
    }
}

async fn run(cli: Cli) -> Result<(), i32> {
    let service = match connect_service().await {
        Ok(service) => service,
        Err(code) => return Err(code),
    };

    match cli.command {
        Commands::Tenant {
            command: TenantCommands::Create { name },
        } => match service.create_tenant(&name).await {
            Ok(created) => print_json(&created),
            Err(err) => fail(err),
        },
        Commands::Key {
            command:
                KeyCommands::Issue {
                    client_id,
                    kind,
                    name,
                },
        } => match service.issue_key(client_id, kind.into(), &name).await {
            Ok(issued) => print_json(&issued),
            Err(err) => fail(err),
        },
    }
}

async fn connect_service() -> Result<ProvisioningService, i32> {
    let config = match DatabasePoolConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}");
            return Err(1);
        }
    };
    let role =
        std::env::var("OPMUX_DB_ROLE").unwrap_or_else(|_| "opmux_operator".to_string());
    let pool = match config.connect_with_role(&role).await {
        Ok(pool) => pool,
        Err(_) => {
            eprintln!("database role is invalid or unavailable");
            return Err(1);
        }
    };
    Ok(ProvisioningService::new(Arc::new(PostgresAuthStore::new(
        pool,
    ))))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), i32> {
    match serde_json::to_string(value) {
        Ok(json) => {
            println!("{json}");
            Ok(())
        }
        Err(_) => {
            eprintln!("failed to serialize issuance result");
            Err(1)
        }
    }
}

fn fail(err: ProvisionError) -> Result<(), i32> {
    eprintln!("{err}");
    Err(1)
}
