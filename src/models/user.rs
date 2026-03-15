use std::sync::Arc;

use rusqlite::{Connection, Error};
use tokio::sync::Mutex;

use crate::error::InternalError;

#[derive(Clone)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub is_admin: bool,
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
            is_admin: false,
        })
    }

    pub async fn delete(self, connection: Arc<Mutex<Connection>>) -> Result<(), InternalError> {
        let now = chrono::Utc::now().timestamp();
        let conn = connection.lock().await;
        conn.execute(
            "UPDATE user SET username = 'Deleted account', provider = '', external_id = '' WHERE id = (?1)",
            (self.id,),
        )
        .map_err(|err| InternalError::new(format!("Failed to anonymize user: {err}")))?;
        conn.execute(
            "DELETE FROM booking WHERE user_id = (?1) AND end_timestamp > (?2)",
            (self.id, now),
        )
        .map_err(|err| InternalError::new(format!("Failed to delete upcoming bookings: {err}")))?;
        conn.execute("DELETE FROM session WHERE user_id = (?1)", (self.id,))
            .map_err(|err| InternalError::new(format!("Failed to delete sessions: {err}")))?;
        Ok(())
    }

    pub async fn fetch_all(connection: Arc<Mutex<Connection>>) -> Result<Vec<User>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, username, provider FROM user ORDER BY id ASC")
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let users = stmt
            .query_map([], |row| {
                Ok(User {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider: row.get(2).unwrap_or_default(),
                    is_admin: false,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query users: {err}")))?;
        let mut res = Vec::new();
        for user in users {
            res.push(user.map_err(|err| InternalError::new(format!("Failed to map row: {err}")))?);
        }
        Ok(res)
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
            Ok((id, name)) => Ok(Some(User {
                id,
                name,
                provider,
                is_admin: false,
            })),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(InternalError::new(format!(
                "Failed to fetch user from db: {err}"
            ))),
        }
    }
}
