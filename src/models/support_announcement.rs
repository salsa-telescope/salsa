use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Mutex;

use crate::error::InternalError;

pub async fn fetch_support_announcement(
    connection: Arc<Mutex<Connection>>,
) -> Result<Option<String>, InternalError> {
    let conn = connection.lock().await;
    let message = conn
        .query_row(
            "SELECT message FROM support_announcement WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|err| InternalError::new(format!("Failed to fetch announcement: {err}")))?;
    Ok(message)
}

pub async fn set_support_announcement(
    connection: Arc<Mutex<Connection>>,
    message: Option<&str>,
) -> Result<(), InternalError> {
    let conn = connection.lock().await;
    match message {
        Some(text) if !text.trim().is_empty() => {
            conn.execute(
                "INSERT INTO support_announcement (id, message) VALUES (1, ?1) \
                 ON CONFLICT(id) DO UPDATE SET message = excluded.message",
                (text.trim(),),
            )
            .map_err(|err| InternalError::new(format!("Failed to save announcement: {err}")))?;
        }
        _ => {
            conn.execute("DELETE FROM support_announcement WHERE id = 1", [])
                .map_err(|err| {
                    InternalError::new(format!("Failed to clear announcement: {err}"))
                })?;
        }
    }
    Ok(())
}
