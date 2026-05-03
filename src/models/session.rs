use base64::{Engine, prelude::BASE64_STANDARD};
use chrono::Utc;
use oauth2::CsrfToken;
use rand::Rng;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{error::InternalError, models::user::User};

/// How long a session cookie is valid for. Matches the cookie Max-Age set on
/// login, and is enforced server-side so a leaked token can't be replayed
/// indefinitely.
pub const SESSION_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;

/// How long a pending OAuth2 CSRF token stays valid. The user has this long
/// to bounce off the provider and come back to the callback.
pub const OAUTH2_PENDING_LIFETIME_SECS: i64 = 15 * 60;

fn generate_random_bytes(num_bytes: usize) -> Vec<u8> {
    let mut result = vec![0; num_bytes];
    rand::rng().fill(result.as_mut_slice());
    result
}

fn create_session_token() -> String {
    BASE64_STANDARD.encode(generate_random_bytes(20))
}

pub async fn start_oauth2_login(
    connection: Arc<Mutex<Connection>>,
    provider: &str,
    csrf_token: &CsrfToken,
) -> Result<(), InternalError> {
    let conn = connection.lock().await;
    conn.execute(
        "INSERT INTO pending_oauth2 (csrf_token, provider, created_at) VALUES ((?1), (?2), (?3))",
        (csrf_token.secret(), provider, Utc::now().timestamp()),
    )
    .map_err(|err| {
        InternalError::new(format!(
            "Failed to insert pending oauth2 action in db: {err}"
        ))
    })?;
    Ok(())
}

pub async fn complete_oauth2_login(
    connection: Arc<Mutex<Connection>>,
    csrf_token: &str,
) -> Result<String, InternalError> {
    let conn = connection.lock().await;
    let oldest_allowed = Utc::now().timestamp() - OAUTH2_PENDING_LIFETIME_SECS;
    let (id, provider) = conn
        .query_row(
            "SELECT id, provider FROM pending_oauth2 \
             WHERE csrf_token = (?1) AND created_at > (?2)",
            (csrf_token, oldest_allowed),
            |row| {
                Ok((
                    row.get::<usize, i64>(0)
                        .expect("Table 'pending_oauth2' has known layout"),
                    row.get::<usize, String>(1)
                        .expect("Table 'pending_oauth2' has known layout"),
                ))
            },
        )
        .map_err(|err| InternalError::new(format!("No pending oauth login found: {err}")))?;
    conn.execute("DELETE FROM pending_oauth2 WHERE id = (?1)", (id,))
        .map_err(|err| {
            InternalError::new(format!(
                "Failed to clear complete pending oauth2 action in db: {err}"
            ))
        })?;

    Ok(provider)
}

/// Delete pending OAuth2 rows past their TTL. Called at startup to keep the
/// table from accumulating abandoned flows over time.
pub async fn purge_expired_pending_oauth2(
    connection: Arc<Mutex<Connection>>,
) -> Result<(), InternalError> {
    let conn = connection.lock().await;
    let oldest_allowed = Utc::now().timestamp() - OAUTH2_PENDING_LIFETIME_SECS;
    conn.execute(
        "DELETE FROM pending_oauth2 WHERE created_at <= (?1)",
        (oldest_allowed,),
    )
    .map_err(|err| InternalError::new(format!("Failed to purge pending oauth2: {err}")))?;
    Ok(())
}

/// Delete sessions past their TTL. Called at startup so leaked-but-unused
/// tokens don't sit around indefinitely.
pub async fn purge_expired_sessions(
    connection: Arc<Mutex<Connection>>,
) -> Result<(), InternalError> {
    let conn = connection.lock().await;
    let oldest_allowed = Utc::now().timestamp() - SESSION_LIFETIME_SECS;
    conn.execute(
        "DELETE FROM session WHERE created_at <= (?1)",
        (oldest_allowed,),
    )
    .map_err(|err| InternalError::new(format!("Failed to purge sessions: {err}")))?;
    Ok(())
}

pub struct Session {
    pub token: String,
    pub user: User,
}

impl Session {
    pub async fn fetch(
        connection: Arc<Mutex<Connection>>,
        token: &str,
    ) -> Result<Option<Session>, InternalError> {
        let conn = connection.lock().await;
        let oldest_allowed = Utc::now().timestamp() - SESSION_LIFETIME_SECS;
        match conn.query_row(
            "SELECT token, user.id, username, provider FROM session \
             INNER JOIN user ON session.user_id = user.id \
             WHERE session.token = (?1) AND session.created_at > (?2)",
            (token, oldest_allowed),
            |row| {
                Ok((
                    row.get::<usize, String>(0)
                        .expect("Table 'session' has known layout"),
                    row.get::<usize, i64>(1)
                        .expect("Table 'user' has known layout"),
                    row.get::<usize, String>(2)
                        .expect("Table 'user' has known layout"),
                    row.get::<usize, String>(3)
                        .expect("Table 'user' has known layout"),
                ))
            },
        ) {
            Ok((token, user_id, username, provider)) => Ok(Some(Session {
                token: token.to_string(),
                user: User {
                    id: user_id,
                    name: username,
                    provider,
                    is_admin: false,
                },
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(InternalError::new(format!(
                "Failed to fetch session from db: {err}"
            ))),
        }
    }

    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: &User,
    ) -> Result<Session, InternalError> {
        let conn = connection.lock().await;
        let token = create_session_token();
        conn.execute(
            "INSERT INTO session (token, user_id, created_at) VALUES ((?1), (?2), (?3))",
            (&token, &user.id, Utc::now().timestamp()),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert session in db: {err}")))?;

        Ok(Session {
            token: token.to_string(),
            user: user.clone(),
        })
    }

    pub async fn delete(self, connection: Arc<Mutex<Connection>>) -> Result<(), InternalError> {
        let conn = connection.lock().await;

        conn.execute("DELETE FROM session WHERE token = (?1)", (self.token,))
            .map_err(|err| {
                InternalError::new(format!(
                    "Failed to clear complete pending oauth2 action in db: {err}"
                ))
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::database::{SqliteDatabaseError, apply_migrations};
    use crate::models::user::User;
    fn create_connection() -> Result<Arc<Mutex<Connection>>, SqliteDatabaseError> {
        let mut connection = Connection::open_in_memory()?;
        apply_migrations(&mut connection)?;
        Ok(Arc::new(Mutex::new(connection)))
    }

    #[tokio::test]
    async fn test_complete_oauth2_with_incorrect_csrf_token_fails() {
        let connection = create_connection().unwrap();
        start_oauth2_login(connection.clone(), "test", &CsrfToken::new_random())
            .await
            .unwrap();
        assert!(
            complete_oauth2_login(connection, CsrfToken::new_random().secret())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_complete_oauth2_clears_request() {
        let connection = create_connection().unwrap();
        let csrf_token = CsrfToken::new_random();
        start_oauth2_login(connection.clone(), "test", &csrf_token)
            .await
            .unwrap();
        // First completion is valid
        assert_eq!(
            "test",
            complete_oauth2_login(connection.clone(), csrf_token.secret())
                .await
                .unwrap()
        );
        // Second fails since the request is cleared
        assert!(
            complete_oauth2_login(connection.clone(), csrf_token.secret())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_create_session() {
        let connection = create_connection().unwrap();
        let user = User::create_from_external(
            connection.clone(),
            "test".to_string(),
            "test".to_string(),
            "1",
        )
        .await
        .unwrap();
        let created_session = Session::create(connection.clone(), &user).await.unwrap();
        let fetched_sesssion = Session::fetch(connection.clone(), &created_session.token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(created_session.token, fetched_sesssion.token);
    }

    #[tokio::test]
    async fn test_expired_session_is_rejected() {
        let connection = create_connection().unwrap();
        let user = User::create_from_external(
            connection.clone(),
            "test".to_string(),
            "test".to_string(),
            "1",
        )
        .await
        .unwrap();
        let session = Session::create(connection.clone(), &user).await.unwrap();
        // Backdate the session past the lifetime
        let stale = Utc::now().timestamp() - SESSION_LIFETIME_SECS - 1;
        connection
            .lock()
            .await
            .execute(
                "UPDATE session SET created_at = (?1) WHERE token = (?2)",
                (stale, &session.token),
            )
            .unwrap();
        assert!(
            Session::fetch(connection.clone(), &session.token)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_expired_pending_oauth2_is_rejected() {
        let connection = create_connection().unwrap();
        let csrf_token = CsrfToken::new_random();
        start_oauth2_login(connection.clone(), "test", &csrf_token)
            .await
            .unwrap();
        let stale = Utc::now().timestamp() - OAUTH2_PENDING_LIFETIME_SECS - 1;
        connection
            .lock()
            .await
            .execute(
                "UPDATE pending_oauth2 SET created_at = (?1) WHERE csrf_token = (?2)",
                (stale, csrf_token.secret()),
            )
            .unwrap();
        assert!(
            complete_oauth2_login(connection, csrf_token.secret())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_purge_expired_sessions_removes_old_rows() {
        let connection = create_connection().unwrap();
        let user = User::create_from_external(
            connection.clone(),
            "test".to_string(),
            "test".to_string(),
            "1",
        )
        .await
        .unwrap();
        let session = Session::create(connection.clone(), &user).await.unwrap();
        let stale = Utc::now().timestamp() - SESSION_LIFETIME_SECS - 1;
        connection
            .lock()
            .await
            .execute(
                "UPDATE session SET created_at = (?1) WHERE token = (?2)",
                (stale, &session.token),
            )
            .unwrap();
        purge_expired_sessions(connection.clone()).await.unwrap();
        let count: i64 = connection
            .lock()
            .await
            .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
