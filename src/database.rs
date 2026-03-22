use deadpool_sqlite::{CreatePoolError, PoolError};
use rusqlite::Connection;
use std::path::PathBuf;
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum SqliteDatabaseError {
    #[error("Could not open database: {source}")]
    Rusqlite {
        #[from]
        source: rusqlite::Error,
    },
    #[error("Could not setup database connection pool: {source}")]
    CreatePool {
        #[from]
        source: CreatePoolError,
    },
    #[error("Could not setup database connection pool: {source}")]
    Pool {
        #[from]
        source: PoolError,
    },
}

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("./sql_migrations");
}

pub fn apply_migrations(connection: &mut Connection) -> Result<(), SqliteDatabaseError> {
    let report = embedded::migrations::runner().run(connection).unwrap();
    debug!("Applied migrations\n{:?}", report);
    Ok(())
}

pub fn create_sqlite_database_on_disk(
    file_path: impl Into<PathBuf>,
) -> Result<Connection, SqliteDatabaseError> {
    let file_path = file_path.into();
    let mut connection = Connection::open(&file_path)?;
    apply_migrations(&mut connection)?;
    Ok(connection)
}
