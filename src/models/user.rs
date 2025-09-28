use std::sync::Arc;

use rusqlite::{Connection, Error};
use tokio::sync::Mutex;

use crate::error::InternalError;

#[derive(Clone)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub provider: String,
}

impl User {
    pub async fn create_from_external(
        connection: Arc<Mutex<Connection>>,
        name: String,
        provider: String,
        external_id: &str,
    ) -> Result<User, InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO user (username, provider, external_id) values ((?1), (?2), (?3))",
            (&name, &provider, external_id),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert user in db: {err}")))?;
        Ok(User {
            id: conn.last_insert_rowid(),
            name,
            provider,
        })
    }

    pub async fn fetch_with_user_with_external_id(
        connection: Arc<Mutex<Connection>>,
        provider: String,
        discord_id: &str,
    ) -> Result<Option<User>, InternalError> {
        let conn = connection.lock().await;
        match conn.query_row(
            "SELECT * FROM user WHERE provider = (?1) AND external_id = (?2)",
            ((&provider), (discord_id)),
            |row| {
                Ok((
                    row.get::<usize, i64>(0)
                        .expect("Table 'user' has known layout"),
                    row.get::<usize, String>(1)
                        .expect("Table 'user' has known layout"),
                ))
            },
        ) {
            Ok((id, name)) => Ok(Some(User { id, name, provider })),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(InternalError::new(format!(
                "Failed to fetch user from db: {err}"
            ))),
        }
    }
}
