use std::sync::Arc;

use chrono::{DateTime, Utc};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::session::Session;
use crate::models::user::User;

/// Total wall-clock cap on a guest session. Any session that runs past this
/// is force-ended at the next idle-release tick regardless of activity.
pub const GUEST_SESSION_HARD_CEILING_SECS: i64 = 30 * 60;

/// If no meaningful telescope command (set_target / start_observe / etc.) has
/// been issued in this many seconds, the session is released. Slewing counts
/// as activity (`set_target` triggers a touch), so 3 minutes is a reasonable
/// quiet-period threshold even given that long slews can take ~4 min.
pub const GUEST_IDLE_RELEASE_SECS: i64 = 3 * 60;

/// How far ahead of `now` we look for upcoming non-guest bookings when
/// deciding whether to allow a guest start. A guest must not be allowed to
/// take a telescope when a real user has it reserved within this window —
/// they shouldn't be evicted seconds after starting.
pub const GUEST_START_PROTECT_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// No telescope command in `GUEST_IDLE_RELEASE_SECS`.
    Idle,
    /// Reached `GUEST_SESSION_HARD_CEILING_SECS`.
    Ceiling,
    /// User clicked "End session".
    User,
    /// A real (non-guest) booking started on this telescope.
    Preempted,
}

impl EndReason {
    fn as_db_str(&self) -> &'static str {
        match self {
            EndReason::Idle => "idle",
            EndReason::Ceiling => "ceiling",
            EndReason::User => "user",
            EndReason::Preempted => "preempted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuestSession {
    pub id: i64,
    pub user_id: i64,
    pub telescope_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    pub end_reason: Option<String>,
    pub country: Option<String>,
}

#[derive(Debug)]
pub enum StartError {
    /// Telescope is currently held by another non-guest booking, or one starts
    /// within `GUEST_START_PROTECT_SECS`.
    TelescopeBusy,
    /// Another guest already holds this telescope.
    GuestAlreadyActive,
    /// SQL/transaction failure.
    Internal(InternalError),
}

impl From<InternalError> for StartError {
    fn from(e: InternalError) -> Self {
        StartError::Internal(e)
    }
}

fn random_guest_username() -> String {
    let suffix: u32 = rand::rng().random();
    format!("guest-{suffix:08x}")
}

impl GuestSession {
    /// Atomically:
    ///   1. Verify no conflicting non-guest booking now or within
    ///      `GUEST_START_PROTECT_SECS`.
    ///   2. Verify no other active guest session on this telescope.
    ///   3. Create a synthetic guest user (`provider='guest'`).
    ///   4. Insert the guest_session row.
    ///   5. Create a session token bound to that user.
    ///
    /// Returns the new user, the active guest session row, and the cookie
    /// token to set in the response.
    pub async fn start(
        connection: Arc<Mutex<Connection>>,
        telescope_id: &str,
        country: Option<String>,
    ) -> Result<(User, GuestSession, Session), StartError> {
        let username = random_guest_username();
        let now_ts = Utc::now().timestamp();
        let conflict_until_ts = now_ts + GUEST_START_PROTECT_SECS;

        let mut conn_guard = connection.lock().await;
        let tx = conn_guard
            .transaction()
            .map_err(|e| InternalError::new(format!("Failed to begin transaction: {e}")))?;

        // 1. Real booking conflict check.
        let real_conflict: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM booking
                 JOIN user ON booking.user_id = user.id
                 WHERE booking.telescope_id = ?1
                   AND user.provider != 'guest'
                   AND booking.start_timestamp < ?2
                   AND booking.end_timestamp > ?3",
                (telescope_id, conflict_until_ts, now_ts),
                |r| r.get(0),
            )
            .map_err(|e| InternalError::new(format!("Failed to check booking conflict: {e}")))?;
        if real_conflict > 0 {
            return Err(StartError::TelescopeBusy);
        }

        // 2. Active guest conflict check.
        let guest_conflict: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM guest_session
                 WHERE telescope_id = ?1 AND ended_at IS NULL",
                (telescope_id,),
                |r| r.get(0),
            )
            .map_err(|e| InternalError::new(format!("Failed to check guest conflict: {e}")))?;
        if guest_conflict > 0 {
            return Err(StartError::GuestAlreadyActive);
        }

        // 3. Insert synthetic user.
        tx.execute(
            "INSERT INTO user (username, provider, external_id) VALUES (?1, 'guest', NULL)",
            (&username,),
        )
        .map_err(|e| InternalError::new(format!("Failed to insert guest user: {e}")))?;
        let user_id = tx.last_insert_rowid();

        // 4. Insert guest_session row.
        tx.execute(
            "INSERT INTO guest_session
                 (user_id, telescope_id, started_at, last_activity_at, country)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            (user_id, telescope_id, now_ts, &country),
        )
        .map_err(|e| InternalError::new(format!("Failed to insert guest_session: {e}")))?;
        let session_id = tx.last_insert_rowid();

        // 5. Insert auth session token. Inlined here (rather than calling
        // Session::create) so the whole operation is one transaction.
        let token = create_session_token();
        tx.execute(
            "INSERT INTO session (token, user_id, created_at) VALUES (?1, ?2, ?3)",
            (&token, user_id, now_ts),
        )
        .map_err(|e| InternalError::new(format!("Failed to insert session: {e}")))?;

        tx.commit()
            .map_err(|e| InternalError::new(format!("Failed to commit transaction: {e}")))?;

        let user = User {
            id: user_id,
            name: username,
            provider: "guest".to_string(),
            is_admin: false,
        };
        let started_at = DateTime::<Utc>::from_timestamp(now_ts, 0).unwrap_or_default();
        let session = Session {
            token: token.clone(),
            user: user.clone(),
        };
        Ok((
            user,
            GuestSession {
                id: session_id,
                user_id,
                telescope_id: telescope_id.to_string(),
                started_at,
                ended_at: None,
                last_activity_at: started_at,
                end_reason: None,
                country,
            },
            session,
        ))
    }

    /// Reset the idle clock for a guest user. No-op if `user_id` is not a
    /// guest or has no active session — callers can fire this from any
    /// telescope-command handler without a prior provider check.
    pub async fn touch_activity(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "UPDATE guest_session SET last_activity_at = ?1
             WHERE user_id = ?2 AND ended_at IS NULL",
            (Utc::now().timestamp(), user_id),
        )
        .map_err(|e| InternalError::new(format!("Failed to touch guest activity: {e}")))?;
        Ok(())
    }

    /// Mark a guest session as ended. Idempotent: a session already ended
    /// stays at its first end_reason (won't be overwritten).
    pub async fn end(
        connection: Arc<Mutex<Connection>>,
        id: i64,
        reason: EndReason,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "UPDATE guest_session SET ended_at = ?1, end_reason = ?2
             WHERE id = ?3 AND ended_at IS NULL",
            (Utc::now().timestamp(), reason.as_db_str(), id),
        )
        .map_err(|e| InternalError::new(format!("Failed to end guest session: {e}")))?;
        Ok(())
    }

    pub async fn fetch_active_for_telescope(
        connection: Arc<Mutex<Connection>>,
        telescope_id: &str,
    ) -> Result<Option<GuestSession>, InternalError> {
        let conn = connection.lock().await;
        conn.query_row(
            "SELECT id, user_id, telescope_id, started_at, ended_at, last_activity_at,
                    end_reason, country
             FROM guest_session
             WHERE telescope_id = ?1 AND ended_at IS NULL",
            (telescope_id,),
            map_row,
        )
        .optional()
        .map_err(|e| InternalError::new(format!("Failed to fetch active guest by telescope: {e}")))
    }

    pub async fn fetch_active_for_user(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<Option<GuestSession>, InternalError> {
        let conn = connection.lock().await;
        conn.query_row(
            "SELECT id, user_id, telescope_id, started_at, ended_at, last_activity_at,
                    end_reason, country
             FROM guest_session
             WHERE user_id = ?1 AND ended_at IS NULL",
            (user_id,),
            map_row,
        )
        .optional()
        .map_err(|e| InternalError::new(format!("Failed to fetch active guest by user: {e}")))
    }

    pub async fn fetch_in_range(
        connection: Arc<Mutex<Connection>>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<GuestSession>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, telescope_id, started_at, ended_at, last_activity_at,
                        end_reason, country
                 FROM guest_session
                 WHERE started_at >= ?1 AND started_at < ?2
                 ORDER BY started_at ASC",
            )
            .map_err(|e| InternalError::new(format!("Failed to prepare statement: {e}")))?;
        stmt.query_map((from.timestamp(), to.timestamp()), map_row)
            .map_err(|e| InternalError::new(format!("Failed to query_map: {e}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect()
    }
}

/// Returns true if `user` is a guest with an active session on `telescope_id`.
/// Mirrors the role of `booking_is_active` for the booking table; callers in
/// observe handlers should accept either as authorisation.
pub async fn guest_is_active(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    telescope_id: &str,
) -> Result<bool, InternalError> {
    if user.provider != "guest" {
        return Ok(false);
    }
    let conn = connection.lock().await;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM guest_session
             WHERE user_id = ?1 AND telescope_id = ?2 AND ended_at IS NULL",
            (user.id, telescope_id),
            |r| r.get(0),
        )
        .map_err(|e| InternalError::new(format!("Failed to check guest active: {e}")))?;
    Ok(count > 0)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GuestSession> {
    Ok(GuestSession {
        id: row.get(0)?,
        user_id: row.get(1)?,
        telescope_id: row.get(2)?,
        started_at: DateTime::<Utc>::from_timestamp(row.get(3)?, 0).unwrap_or_default(),
        ended_at: row
            .get::<_, Option<i64>>(4)?
            .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
        last_activity_at: DateTime::<Utc>::from_timestamp(row.get(5)?, 0).unwrap_or_default(),
        end_reason: row.get(6)?,
        country: row.get(7)?,
    })
}

// Inlined here so `start` can do the user-create + guest_session + session
// inserts in one transaction without going through the Session::create API
// (which takes its own connection lock).
fn create_session_token() -> String {
    use base64::{Engine, prelude::BASE64_STANDARD};
    let mut bytes = [0u8; 20];
    rand::rng().fill(&mut bytes);
    BASE64_STANDARD.encode(bytes)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::database::{SqliteDatabaseError, apply_migrations};
    use chrono::Duration;

    fn create_connection() -> Result<Arc<Mutex<Connection>>, SqliteDatabaseError> {
        let mut connection = Connection::open_in_memory()?;
        apply_migrations(&mut connection)?;
        Ok(Arc::new(Mutex::new(connection)))
    }

    async fn create_real_user(conn: &Arc<Mutex<Connection>>) -> User {
        User::create_from_external(
            conn.clone(),
            "real".to_string(),
            "google".to_string(),
            "ext-1",
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn start_succeeds_on_free_telescope() {
        let conn = create_connection().unwrap();
        let (user, gs, _session) = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        assert_eq!(user.provider, "guest");
        assert!(user.name.starts_with("guest-"));
        assert_eq!(gs.telescope_id, "vale");
        assert!(gs.ended_at.is_none());
    }

    #[tokio::test]
    async fn start_refuses_when_real_booking_active() {
        let conn = create_connection().unwrap();
        let real = create_real_user(&conn).await;
        let now = Utc::now();
        crate::models::booking::Booking::create(
            conn.clone(),
            real,
            "vale".to_string(),
            now - Duration::minutes(10),
            now + Duration::minutes(50),
            None,
            None,
        )
        .await
        .unwrap();

        match GuestSession::start(conn.clone(), "vale", None).await {
            Err(StartError::TelescopeBusy) => {}
            other => panic!("expected TelescopeBusy, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_refuses_when_real_booking_starts_within_protect_window() {
        let conn = create_connection().unwrap();
        let real = create_real_user(&conn).await;
        let now = Utc::now();
        // Booking starts in 2 minutes — inside the 5-min protect window.
        crate::models::booking::Booking::create(
            conn.clone(),
            real,
            "vale".to_string(),
            now + Duration::minutes(2),
            now + Duration::minutes(62),
            None,
            None,
        )
        .await
        .unwrap();

        match GuestSession::start(conn.clone(), "vale", None).await {
            Err(StartError::TelescopeBusy) => {}
            other => panic!("expected TelescopeBusy, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_allows_when_real_booking_far_in_future() {
        let conn = create_connection().unwrap();
        let real = create_real_user(&conn).await;
        let now = Utc::now();
        // Booking starts in 30 minutes — well outside the protect window.
        crate::models::booking::Booking::create(
            conn.clone(),
            real,
            "vale".to_string(),
            now + Duration::minutes(30),
            now + Duration::minutes(90),
            None,
            None,
        )
        .await
        .unwrap();

        let result = GuestSession::start(conn.clone(), "vale", None).await;
        assert!(result.is_ok(), "expected ok, got {:?}", result.err());
    }

    #[tokio::test]
    async fn start_refuses_second_concurrent_guest() {
        let conn = create_connection().unwrap();
        let _first = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        match GuestSession::start(conn.clone(), "vale", None).await {
            Err(StartError::GuestAlreadyActive) => {}
            other => panic!("expected GuestAlreadyActive, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_allows_guest_on_different_telescope() {
        let conn = create_connection().unwrap();
        let _first = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        let second = GuestSession::start(conn.clone(), "brage", None).await;
        assert!(second.is_ok(), "expected ok, got {:?}", second.err());
    }

    #[tokio::test]
    async fn touch_activity_updates_timestamp() {
        let conn = create_connection().unwrap();
        let (user, gs, _) = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();

        // Backdate last_activity_at to simulate idle.
        let stale = Utc::now().timestamp() - 1000;
        conn.lock()
            .await
            .execute(
                "UPDATE guest_session SET last_activity_at = ?1 WHERE id = ?2",
                (stale, gs.id),
            )
            .unwrap();

        GuestSession::touch_activity(conn.clone(), user.id)
            .await
            .unwrap();

        let refreshed = GuestSession::fetch_active_for_user(conn.clone(), user.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            refreshed.last_activity_at.timestamp() > stale + 100,
            "expected last_activity_at to be refreshed",
        );
    }

    #[tokio::test]
    async fn end_marks_session_inactive_with_reason() {
        let conn = create_connection().unwrap();
        let (_user, gs, _) = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        GuestSession::end(conn.clone(), gs.id, EndReason::User)
            .await
            .unwrap();
        // No active session on the telescope after end.
        let active = GuestSession::fetch_active_for_telescope(conn.clone(), "vale")
            .await
            .unwrap();
        assert!(active.is_none());

        // Range query still finds the row with the right reason.
        let now = Utc::now();
        let rows = GuestSession::fetch_in_range(
            conn.clone(),
            now - Duration::minutes(1),
            now + Duration::minutes(1),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].end_reason.as_deref(), Some("user"));
    }

    #[tokio::test]
    async fn end_is_idempotent_first_reason_wins() {
        let conn = create_connection().unwrap();
        let (_user, gs, _) = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        GuestSession::end(conn.clone(), gs.id, EndReason::Idle)
            .await
            .unwrap();
        // Second end with a different reason should not overwrite.
        GuestSession::end(conn.clone(), gs.id, EndReason::Preempted)
            .await
            .unwrap();
        let now = Utc::now();
        let rows = GuestSession::fetch_in_range(
            conn.clone(),
            now - Duration::minutes(1),
            now + Duration::minutes(1),
        )
        .await
        .unwrap();
        assert_eq!(rows[0].end_reason.as_deref(), Some("idle"));
    }

    #[tokio::test]
    async fn guest_is_active_only_for_matching_user_and_telescope() {
        let conn = create_connection().unwrap();
        let (user, _, _) = GuestSession::start(conn.clone(), "vale", None)
            .await
            .unwrap();
        assert!(guest_is_active(conn.clone(), &user, "vale").await.unwrap());
        assert!(!guest_is_active(conn.clone(), &user, "brage").await.unwrap());

        // Non-guest user always returns false even on the right telescope.
        let real = create_real_user(&conn).await;
        assert!(!guest_is_active(conn.clone(), &real, "vale").await.unwrap());
    }
}
