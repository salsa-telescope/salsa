use crate::coords::vlsrcorr_from_galactic;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, info, warn};

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
    let runner = embedded::migrations::runner();
    let report = runner.run(connection).unwrap();
    debug!("Applied migrations\n{:?}", report);
    // refinery's own info lines are filtered out in `logging` because they
    // call the schema version just "version", easily mistaken for the
    // release. These say what the number is.
    for migration in report.applied_migrations() {
        info!(
            "applied database migration V{}__{}",
            migration.version(),
            migration.name()
        );
    }
    if let Some(last) = runner.get_last_applied_migration(connection).unwrap() {
        info!("database schema at migration V{}", last.version());
    }
    run_pending_data_fixups(connection)?;
    Ok(())
}

/// Runs fixups queued in `pending_data_fixup` by a migration (see V20), each
/// in one transaction together with removing its row, so it runs exactly once.
fn run_pending_data_fixups(connection: &mut Connection) -> Result<(), SqliteDatabaseError> {
    let pending: Vec<String> = connection
        .prepare("SELECT name FROM pending_data_fixup")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for name in pending {
        let tx = connection.transaction()?;
        match name.as_str() {
            "recompute_vlsr_correction" => recompute_vlsr_correction(&tx)?,
            other => {
                // Leave the row: a newer binary may know it.
                warn!("unknown pending data fixup {other:?}, skipping");
                continue;
            }
        }
        tx.execute("DELETE FROM pending_data_fixup WHERE name = ?1", [&name])?;
        tx.commit()?;
    }
    Ok(())
}

/// Recomputes the stored VLSR correction of every galactic observation; see
/// V20__recompute_vlsr_correction.sql for why.
fn recompute_vlsr_correction(tx: &rusqlite::Transaction) -> Result<(), SqliteDatabaseError> {
    let rows: Vec<(i64, i64, f64, f64, Option<f64>)> = tx
        .prepare(
            "SELECT id, start_time, target_x, target_y, vlsr_correction_mps \
             FROM observation WHERE coordinate_system = 'galactic'",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut update = tx.prepare("UPDATE observation SET vlsr_correction_mps = ?1 WHERE id = ?2")?;
    let mut changed = 0;
    for (id, start_time, l_deg, b_deg, old) in &rows {
        let Some(when) = DateTime::<Utc>::from_timestamp(*start_time, 0) else {
            continue;
        };
        let new = vlsrcorr_from_galactic(l_deg.to_radians(), b_deg.to_radians(), when);
        // Start times are stored to the second, so unaffected rows shift by
        // well under 1 m/s; only count the ones the bug changed.
        if old.is_none_or(|old| (old - new).abs() > 1.0) {
            changed += 1;
        }
        update.execute(rusqlite::params![new, id])?;
    }
    info!(
        "recomputed VLSR correction for {} galactic observations, {} changed",
        rows.len(),
        changed
    );
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

#[cfg(test)]
mod test {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn recompute_vlsr_correction_fixes_galactic_rows_once() {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&mut connection).unwrap();
        // No user row needed for this test.
        connection
            .execute_batch("PRAGMA foreign_keys = OFF")
            .unwrap();
        // Simulate rows saved by the old code: the l = 210° observation from
        // 2026-10-26 stored -42.7 km/s instead of ≈ +9.8 km/s.
        let when = Utc.with_ymd_and_hms(2026, 10, 26, 7, 38, 6).unwrap();
        let insert = "INSERT INTO observation (user_id, telescope_id, start_time, \
             coordinate_system, target_x, target_y, integration_time_secs, \
             frequencies_json, amplitudes_json, vlsr_correction_mps) \
             VALUES (1, 'brage', ?1, ?2, 210.0, 0.0, 100.0, '[]', '[]', ?3)";
        connection
            .execute(
                insert,
                rusqlite::params![when.timestamp(), "galactic", -42700.0],
            )
            .unwrap();
        connection
            .execute(
                insert,
                rusqlite::params![when.timestamp(), "horizontal", None::<f64>],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO pending_data_fixup (name) VALUES ('recompute_vlsr_correction')",
                [],
            )
            .unwrap();

        run_pending_data_fixups(&mut connection).unwrap();

        let values: Vec<Option<f64>> = connection
            .prepare("SELECT vlsr_correction_mps FROM observation ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!((values[0].unwrap() - 9800.0).abs() < 300.0, "{values:?}");
        assert_eq!(values[1], None);
        let pending: i64 = connection
            .query_row("SELECT COUNT(*) FROM pending_data_fixup", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pending, 0);
    }
}
