use std::{path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tracing::{error, info};

use backend::logging::setup_logging;
use backend::{database::create_sqlite_database_on_disk, models::user::User};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Adds files to myapp
    AddLocal {
        username: String,

        #[arg(long, env)]
        password: String,

        #[arg(long, default_value = ".")]
        database_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    setup_logging(false);
    match args.command {
        Commands::AddLocal {
            username,
            password,
            database_dir,
        } => {
            let connection =
                create_sqlite_database_on_disk(database_dir.join("database.sqlite3")).unwrap();
            let connection = Arc::new(Mutex::new(connection));
            match User::create_local(connection, username, password, "".to_string()).await {
                Ok(user) => {
                    info!("Added user {:?} to database", user);
                }
                Err(error) => error!("Failed to add user to database: {:?}", error),
            };
        }
    }
}
