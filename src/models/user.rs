use std::sync::Arc;

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use rusqlite::{Connection, Error, OptionalExtension};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::i18n::Language;

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub is_admin: bool,
    /// Preferred IANA timezone for displaying dates and times. `None` until
    /// the user picks one (or it's auto-detected from the browser on first
    /// login); treated as UTC for display via [`User::tz`].
    pub timezone: Option<chrono_tz::Tz>,
    /// Preferred UI language. `None` means no preference — the language
    /// cookie or Accept-Language header decides instead.
    pub language: Option<Language>,
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
    /// Effective display timezone, defaulting to UTC when the user hasn't
    /// chosen one yet.
    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone.unwrap_or(chrono_tz::UTC)
    }

    /// Persist the user's preferred timezone. The value is validated
    /// against the IANA database before storing so we never keep junk, and
    /// the canonical name is written back.
    pub async fn set_timezone(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        timezone: &str,
    ) -> Result<(), InternalError> {
        let tz: chrono_tz::Tz = timezone
            .parse()
            .map_err(|_| InternalError::new(format!("Invalid timezone: {timezone}")))?;
        let conn = connection.lock().await;
        conn.execute(
            "UPDATE user SET timezone = ?1 WHERE id = ?2",
            (tz.name(), user_id),
        )
        .map_err(|e| InternalError::new(format!("Failed to update timezone: {e}")))?;
        Ok(())
    }

    /// Persist the user's preferred UI language. The code is validated
    /// against the supported languages before storing so we never keep
    /// junk, and the canonical code is written back.
    pub async fn set_language(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        language: &str,
    ) -> Result<(), InternalError> {
        let language = Language::from_code(language)
            .ok_or_else(|| InternalError::new(format!("Unsupported language: {language}")))?;
        let conn = connection.lock().await;
        conn.execute(
            "UPDATE user SET language = ?1 WHERE id = ?2",
            (language.code(), user_id),
        )
        .map_err(|e| InternalError::new(format!("Failed to update language: {e}")))?;
        Ok(())
    }

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
            timezone: None,
            language: None,
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
            timezone: None,
            language: None,
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
                timezone: None,
                language: None,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn fetch_all_local(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<(i64, String, String)>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn.prepare(
            "SELECT u.id, u.username, l.comment
                 FROM user u JOIN local_user l ON u.id = l.user_id
                 ORDER BY u.id ASC",
        )?;
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
            result.push(row?);
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
        // Invalidate any existing sessions so a compromised account can't stay
        // logged in after the password is reset.
        conn.execute("DELETE FROM session WHERE user_id = ?1", (user_id,))
            .map_err(|e| {
                InternalError::new(format!(
                    "Failed to invalidate sessions after password change: {e}"
                ))
            })?;
        Ok(())
    }

    pub async fn set_local_comment(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
        new_comment: String,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        let updated = conn
            .execute(
                "UPDATE local_user SET comment = ?1 WHERE user_id = ?2",
                (&new_comment, user_id),
            )
            .map_err(|e| InternalError::new(format!("Failed to update comment: {e}")))?;
        if updated == 0 {
            return Err(InternalError::new("Not a local user".to_string()));
        }
        Ok(())
    }

    pub async fn delete_local_by_id(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<(), InternalError> {
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
        anonymize(&conn, user_id)
    }

    pub async fn delete(self, connection: Arc<Mutex<Connection>>) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        anonymize(&conn, self.id)
    }

    /// Count registered (non-guest) users grouped by authentication
    /// provider, ordered by count descending then provider name. Used by
    /// the admin page to show how many real accounts exist per provider.
    pub async fn count_by_provider_non_guest(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<(String, usize)>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn.prepare(
            "SELECT provider, COUNT(*) FROM user
                 WHERE provider != 'guest'
                 GROUP BY provider
                 ORDER BY COUNT(*) DESC, provider ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
            })
            .map_err(|err| InternalError::new(format!("Failed to query providers: {err}")))?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r?);
        }
        Ok(res)
    }

    /// Every non-guest user, ordered by id. Guests are synthetic per-session
    /// rows that hold no bookings or observations, so they would only add
    /// noise to admin filter dropdowns.
    pub async fn fetch_all_non_guest(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<User>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, username, provider FROM user
                 WHERE provider != 'guest' ORDER BY id ASC",
        )?;
        let users = stmt
            .query_map([], |row| {
                Ok(User {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider: row.get(2).unwrap_or_default(),
                    is_admin: false,
                    timezone: None,
                    language: None,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query users: {err}")))?;
        let mut res = Vec::new();
        for user in users {
            res.push(user?);
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
                timezone: None,
                language: None,
            })),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(InternalError::new(format!(
                "Failed to fetch user from db: {err}"
            ))),
        }
    }
}

/// Delete an account, whether the user asked or an admin did.
///
/// Past bookings, guest sessions and observations are kept, because they
/// are how usage is measured, but nothing left on them leads back to the
/// person: the user row keeps only its id, and free-text booking
/// descriptions (which can name people) are cleared. The country on
/// bookings and guest sessions stays, for the same usage statistics.
/// Upcoming bookings are cancelled and all sessions are logged out.
fn anonymize(conn: &Connection, user_id: i64) -> Result<(), InternalError> {
    let now = chrono::Utc::now().timestamp();
    let run = |what: &str, sql: &str, params: &[&dyn rusqlite::ToSql]| {
        conn.execute(sql, params)
            .map(|_| ())
            .map_err(|e| InternalError::new(format!("Failed to {what}: {e}")))
    };
    run(
        "delete local login",
        "DELETE FROM local_user WHERE user_id = ?1",
        &[&user_id],
    )?;
    run(
        "anonymize user",
        "UPDATE user SET username = 'Deleted account', provider = '', external_id = '',
             timezone = NULL, language = NULL
         WHERE id = ?1",
        &[&user_id],
    )?;
    run(
        "delete upcoming bookings",
        "DELETE FROM booking WHERE user_id = ?1 AND end_timestamp > ?2",
        &[&user_id, &now],
    )?;
    run(
        "clear booking descriptions",
        "UPDATE booking SET description = NULL WHERE user_id = ?1",
        &[&user_id],
    )?;
    run(
        "delete sessions",
        "DELETE FROM session WHERE user_id = ?1",
        &[&user_id],
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::database::apply_migrations;
    use crate::models::booking::Booking;
    use chrono::{Duration, Utc};

    fn create_connection() -> Arc<Mutex<Connection>> {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&mut connection).unwrap();
        Arc::new(Mutex::new(connection))
    }

    #[tokio::test]
    async fn deleting_an_account_leaves_nothing_that_identifies_the_user() {
        let db = create_connection();
        let user = User::create_from_external(db.clone(), "Anna".into(), "github".into(), "gh-1")
            .await
            .unwrap();
        User::set_timezone(db.clone(), user.id, "Europe/Stockholm")
            .await
            .unwrap();
        let now = Utc::now();
        let past = (
            now - Duration::days(2),
            now - Duration::days(2) + Duration::hours(1),
        );
        let future = (
            now + Duration::days(2),
            now + Duration::days(2) + Duration::hours(1),
        );
        for (start, end) in [past, future] {
            Booking::create(
                db.clone(),
                user.clone(),
                "fake1".into(),
                start,
                end,
                Some("Lab for Anna's group".into()),
                Some("SE".into()),
            )
            .await
            .unwrap();
        }
        let id = user.id;
        user.delete(db.clone()).await.unwrap();

        let conn = db.lock().await;
        let row: (String, String, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT username, provider, external_id, timezone, language FROM user WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("Deleted account".into(), "".into(), "".into(), None, None)
        );
        // The past booking stays for usage statistics, country included, but
        // without its description; the upcoming one is cancelled.
        let bookings: Vec<(Option<String>, Option<String>)> = conn
            .prepare("SELECT description, country FROM booking WHERE user_id = ?1")
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(bookings, vec![(None, Some("SE".to_string()))]);
    }

    #[tokio::test]
    async fn a_local_user_deleting_their_account_removes_their_login() {
        let db = create_connection();
        let user = User::create_local(
            db.clone(),
            "anna".into(),
            "hunter22hunter22".into(),
            "".into(),
        )
        .await
        .unwrap();
        let id = user.id;
        user.delete(db.clone()).await.unwrap();
        let logins: i64 = db
            .lock()
            .await
            .query_row(
                "SELECT COUNT(*) FROM local_user WHERE user_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logins, 0);
    }
}
