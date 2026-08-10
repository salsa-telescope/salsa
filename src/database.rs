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
    connection.execute_batch(
        // SQLite disables FK enforcement per connection by default; without this, every
        // REFERENCES / ON DELETE CASCADE clause in the migrations is silently a no-op.
        "PRAGMA foreign_keys = ON;\
         \
         -- WAL is a property of the database file, so this is set once and
         -- persists. The app holds a single connection behind a mutex, so the
         -- reader/writer concurrency WAL is famous for is not what we get from
         -- it here; the wins are commits that no longer rewrite a rollback
         -- journal, and an external `sqlite3` session being able to read the
         -- live database without blocking the server.
         --
         -- Note for anything that copies the database: WAL keeps recent commits
         -- in a `-wal` sidecar until checkpoint, so `cp database.sqlite3` alone
         -- can silently miss them. Use `sqlite3 database.sqlite3 \".backup ...\"`.
         PRAGMA journal_mode = WAL;\
         \
         -- Cannot fire from in-process contention (there is only ever one
         -- connection), but keeps an external writer — a maintenance script, a
         -- manual sqlite3 session — from failing instantly with SQLITE_BUSY.
         PRAGMA busy_timeout = 5000;\
         \
         -- Safe to pair with WAL: a crash cannot corrupt the database, it can
         -- only lose the last commits, which for bookings and archived spectra
         -- is an acceptable trade for not fsyncing on every write.
         PRAGMA synchronous = NORMAL;",
    )?;
    apply_migrations(&mut connection)?;
    Ok(connection)
}
