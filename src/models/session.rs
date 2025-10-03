use base64::{Engine, prelude::BASE64_STANDARD};
use oauth2::CsrfToken;
use rand::Rng;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{error::InternalError, models::user::User};

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
        "INSERT INTO pending_oauth2 (csrf_token, provider) VALUES ((?1), (?2))",
        (csrf_token.secret(), provider),
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
    let (id, provider) = conn
        .query_row(
            "SELECT id, provider FROM pending_oauth2 WHERE csrf_token = (?1)",
            (csrf_token,),
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
        match conn.query_row(
            "SELECT token, user.id, username, provider FROM session \
             INNER JOIN user ON session.user_id = user.id \
             WHERE session.token = (?1)",
            ((token),),
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
            "INSERT INTO session (token, user_id) VALUES ((?1), (?2))",
            (&token, &user.id),
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
            complete_oauth2_login(connection, &CsrfToken::new_random().secret())
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
            complete_oauth2_login(connection.clone(), &csrf_token.secret())
                .await
                .unwrap()
        );
        // Second fails since the request is cleared
        assert!(
            complete_oauth2_login(connection.clone(), &csrf_token.secret())
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
}
