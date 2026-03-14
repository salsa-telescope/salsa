use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::error::InternalError;

pub async fn fetch_maintenance_set(
    connection: Arc<Mutex<Connection>>,
) -> Result<HashSet<String>, InternalError> {
    let conn = connection.lock().await;
    let mut stmt = conn
        .prepare("SELECT telescope_id FROM telescope_maintenance")
        .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| InternalError::new(format!("Failed to query maintenance: {err}")))?;
    let mut set = HashSet::new();
    for id in ids {
        set.insert(
            id.map_err(|err| InternalError::new(format!("Failed to read row: {err}")))?,
        );
    }
    Ok(set)
}

pub async fn set_maintenance(
    connection: Arc<Mutex<Connection>>,
    telescope_id: &str,
    in_maintenance: bool,
) -> Result<(), InternalError> {
    let conn = connection.lock().await;
    if in_maintenance {
        conn.execute(
            "INSERT OR IGNORE INTO telescope_maintenance (telescope_id) VALUES (?1)",
            (telescope_id,),
        )
        .map_err(|err| InternalError::new(format!("Failed to set maintenance: {err}")))?;
    } else {
        conn.execute(
            "DELETE FROM telescope_maintenance WHERE telescope_id = ?1",
            (telescope_id,),
        )
        .map_err(|err| InternalError::new(format!("Failed to clear maintenance: {err}")))?;
    }
    Ok(())
}
