use std::sync::Arc;

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use rusqlite::{Connection, Error, OptionalExtension};
use tokio::sync::Mutex;

use crate::error::InternalError;

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub is_admin: bool,
}

async fn hash_password(password: String) -> Result<String, InternalError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| InternalError::new(format!("Failed to hash password: {e}")))
    })
    .await
    .map_err(|e| InternalError::new(format!("Task join error: {e}")))?
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

    pub async fn create_local(
        connection: Arc<Mutex<Connection>>,
        username: String,
        password: String,
        comment: String,
    ) -> Result<User, InternalError> {
        // Check username is not already taken by another local user.
        {
            let conn = connection.lock().await;
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM user WHERE provider = 'local' AND username = ?1",
                    [&username],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| InternalError::new(format!("Failed to check username: {e}")))?
                > 0;
            if exists {
                return Err(InternalError::new(format!(
                    "Local user '{username}' already exists"
                )));
            }
        }

        let hash = hash_password(password).await?;

        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO user (username, provider, external_id) VALUES (?1, 'local', NULL)",
            [&username],
        )
        .map_err(|e| InternalError::new(format!("Failed to insert user: {e}")))?;
        let user_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO local_user (user_id, password_hash, comment) VALUES (?1, ?2, ?3)",
            (user_id, &hash, &comment),
        )
        .map_err(|e| InternalError::new(format!("Failed to insert local_user: {e}")))?;

        Ok(User {
            id: user_id,
            name: username,
            provider: "local".to_string(),
            is_admin: false,
        })
    }

    pub async fn fetch_local_with_password(
        connection: Arc<Mutex<Connection>>,
        username: &str,
        password: &str,
    ) -> Result<Option<User>, InternalError> {
        let row = {
            let conn = connection.lock().await;
            conn.query_row(
                "SELECT u.id, u.username, l.password_hash
                 FROM user u JOIN local_user l ON u.id = l.user_id
                 WHERE u.username = ?1",
                [username],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| InternalError::new(format!("Failed to query local user: {e}")))?
        };

        let Some((id, name, stored_hash)) = row else {
            return Ok(None);
        };

        let password = password.to_string();
        let valid = tokio::task::spawn_blocking(move || {
            let parsed_hash = PasswordHash::new(&stored_hash)
                .map_err(|e| InternalError::new(format!("Failed to parse hash: {e}")))?;
            Ok::<bool, InternalError>(
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .is_ok(),
            )
        })
        .await
        .map_err(|e| InternalError::new(format!("Task join error: {e}")))??;

        if valid {
            Ok(Some(User {
                id,
                name,
                provider: "local".to_string(),
                is_admin: false,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn fetch_all_local(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<(i64, String, String)>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT u.id, u.username, l.comment
                 FROM user u JOIN local_user l ON u.id = l.user_id
                 ORDER BY u.id ASC",
            )
            .map_err(|e| InternalError::new(format!("Failed to prepare statement: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| InternalError::new(format!("Failed to query local users: {e}")))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| InternalError::new(format!("Failed to map row: {e}")))?);
        }
        Ok(result)
    }

    pub async fn set_local_password(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        new_password: String,
    ) -> Result<(), InternalError> {
        let hash = hash_password(new_password).await?;
        let conn = connection.lock().await;
        let updated = conn
            .execute(
                "UPDATE local_user SET password_hash = ?1 WHERE user_id = ?2",
                (&hash, user_id),
            )
            .map_err(|e| InternalError::new(format!("Failed to update password: {e}")))?;
        if updated == 0 {
            return Err(InternalError::new("Not a local user".to_string()));
        }
        Ok(())
    }

    pub async fn delete_local_by_id(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<(), InternalError> {
        let now = chrono::Utc::now().timestamp();
        let conn = connection.lock().await;
        let is_local: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM local_user WHERE user_id = ?1",
                [user_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| InternalError::new(format!("Failed to check local user: {e}")))?
            > 0;
        if !is_local {
            return Err(InternalError::new("Not a local user".to_string()));
        }
        conn.execute("DELETE FROM local_user WHERE user_id = ?1", [user_id])
            .map_err(|e| InternalError::new(format!("Failed to delete local_user: {e}")))?;
        conn.execute(
            "UPDATE user SET username = 'Deleted account', provider = '', external_id = '' WHERE id = ?1",
            [user_id],
        )
        .map_err(|e| InternalError::new(format!("Failed to anonymize user: {e}")))?;
        conn.execute(
            "DELETE FROM booking WHERE user_id = ?1 AND end_timestamp > ?2",
            (user_id, now),
        )
        .map_err(|e| InternalError::new(format!("Failed to delete bookings: {e}")))?;
        conn.execute("DELETE FROM session WHERE user_id = ?1", [user_id])
            .map_err(|e| InternalError::new(format!("Failed to delete sessions: {e}")))?;
        Ok(())
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
