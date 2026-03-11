use std::sync::Arc;

use rusqlite::{Connection, Error};
use tokio::sync::Mutex;

use crate::error::InternalError;

#[derive(Clone)]
pub struct User {
    pub id: i64,
    pub name: String,
}

pub struct UserIdentity {
    pub provider: String,
}

pub enum LinkResult {
    Linked,
    AlreadyLinkedToSelf,
    AlreadyLinkedToOther,
}

pub enum UnlinkResult {
    Unlinked,
    LastIdentity,
    NotFound,
}

impl User {
    pub async fn create_from_external(
        connection: Arc<Mutex<Connection>>,
        name: String,
        provider: String,
        external_id: &str,
    ) -> Result<User, InternalError> {
        let conn = connection.lock().await;
        conn.execute("INSERT INTO user (username) VALUES (?1)", (&name,))
            .map_err(|err| InternalError::new(format!("Failed to insert user in db: {err}")))?;
        let user_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO user_identity (user_id, provider, external_id) VALUES (?1, ?2, ?3)",
            (&user_id, &provider, external_id),
        )
        .map_err(|err| {
            InternalError::new(format!("Failed to insert user identity in db: {err}"))
        })?;
        Ok(User { id: user_id, name })
    }

    pub async fn fetch_with_user_with_external_id(
        connection: Arc<Mutex<Connection>>,
        provider: String,
        external_id: &str,
    ) -> Result<Option<User>, InternalError> {
        let conn = connection.lock().await;
        match conn.query_row(
            "SELECT user.id, user.username FROM user
             INNER JOIN user_identity ON user.id = user_identity.user_id
             WHERE user_identity.provider = ?1 AND user_identity.external_id = ?2",
            (&provider, external_id),
            |row| {
                Ok((
                    row.get::<usize, i64>(0)
                        .expect("Table 'user' has known layout"),
                    row.get::<usize, String>(1)
                        .expect("Table 'user' has known layout"),
                ))
            },
        ) {
            Ok((id, name)) => Ok(Some(User { id, name })),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(InternalError::new(format!(
                "Failed to fetch user from db: {err}"
            ))),
        }
    }

    pub async fn fetch_identities(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<Vec<UserIdentity>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare("SELECT provider FROM user_identity WHERE user_id = ?1")
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let identities = stmt
            .query_map((&user_id,), |row| {
                Ok(UserIdentity {
                    provider: row.get(0)?,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query identities: {err}")))?;
        let mut result = Vec::new();
        for identity in identities {
            result.push(
                identity
                    .map_err(|err| InternalError::new(format!("Failed to map row: {err}")))?,
            );
        }
        Ok(result)
    }

    pub async fn link_identity(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        provider: &str,
        external_id: &str,
    ) -> Result<LinkResult, InternalError> {
        let conn = connection.lock().await;
        match conn.query_row(
            "SELECT user_id FROM user_identity WHERE provider = ?1 AND external_id = ?2",
            (provider, external_id),
            |row| row.get::<usize, i64>(0),
        ) {
            Ok(existing_user_id) => {
                if existing_user_id == user_id {
                    return Ok(LinkResult::AlreadyLinkedToSelf);
                } else {
                    return Ok(LinkResult::AlreadyLinkedToOther);
                }
            }
            Err(Error::QueryReturnedNoRows) => {}
            Err(err) => {
                return Err(InternalError::new(format!(
                    "Failed to check identity: {err}"
                )));
            }
        }
        conn.execute(
            "INSERT INTO user_identity (user_id, provider, external_id) VALUES (?1, ?2, ?3)",
            (&user_id, provider, external_id),
        )
        .map_err(|err| InternalError::new(format!("Failed to link identity: {err}")))?;
        Ok(LinkResult::Linked)
    }

    pub async fn unlink_identity(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        provider: &str,
    ) -> Result<UnlinkResult, InternalError> {
        let conn = connection.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_identity WHERE user_id = ?1",
                (&user_id,),
                |row| row.get(0),
            )
            .map_err(|err| InternalError::new(format!("Failed to count identities: {err}")))?;
        if count <= 1 {
            return Ok(UnlinkResult::LastIdentity);
        }
        let rows = conn
            .execute(
                "DELETE FROM user_identity WHERE user_id = ?1 AND provider = ?2",
                (&user_id, provider),
            )
            .map_err(|err| InternalError::new(format!("Failed to unlink identity: {err}")))?;
        if rows == 0 {
            return Ok(UnlinkResult::NotFound);
        }
        Ok(UnlinkResult::Unlinked)
    }
}
