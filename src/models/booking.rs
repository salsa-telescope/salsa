use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::user::User;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Booking {
    pub id: i64,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub telescope_name: String,
    pub user_id: i64,
    pub user_name: String,
    pub user_provider: String,
}

impl Booking {
    pub fn overlaps(&self, other: &Booking) -> bool {
        self.start_time < other.end_time && self.end_time > other.start_time
    }

    pub fn active_at(&self, instant: &DateTime<Utc>) -> bool {
        *instant > self.start_time && *instant < self.end_time
    }

    pub async fn delete(
        self,
        connection: Arc<Mutex<Connection>>,
        user: &User,
    ) -> Result<bool, InternalError> {
        let conn = connection.lock().await;
        let rows_deleted = conn
            .execute(
                "DELETE FROM booking
                WHERE id = (?1)
                AND user_id = (?2)",
                (&self.id, &user.id),
            )
            .map_err(|err| {
                InternalError::new(format!("Failed to delete booking from db: {err}"))
            })?;
        assert!(rows_deleted < 2);
        Ok(rows_deleted > 0)
    }

    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: User,
        telescope_id: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(), InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "INSERT INTO booking (user_id, telescope_id, start_timestamp, end_timestamp)
                 VALUES ((?1), (?2), (?3), (?4))",
            (&user.id, &telescope_id, start.timestamp(), end.timestamp()),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert booking in db: {err}")))?;
        Ok(())
    }

    pub async fn fetch_all(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider
                FROM booking, user WHERE booking.user_id = user.id
                ORDER BY start_timestamp ASC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        stmt.query_map([], map_booking_row)
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect()
    }

    pub async fn fetch_for_user(
        connection: Arc<Mutex<Connection>>,
        user: &User,
    ) -> Result<Vec<Booking>, InternalError> {
        Self::fetch_for_user_id(connection, user.id).await
    }

    pub async fn fetch_for_user_id(
        connection: Arc<Mutex<Connection>>,
        user_id: i64,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider
                FROM booking, user WHERE booking.user_id = user.id AND user.id = ?1
                ORDER BY start_timestamp ASC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        stmt.query_map([user_id], map_booking_row)
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect()
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
    ) -> Result<Option<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider
                FROM booking, user WHERE booking.user_id = user.id AND booking.id = ?1
                ORDER BY start_timestamp ASC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        Ok(stmt
            .query_map([id], map_booking_row)
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect::<Result<Vec<_>, _>>()?
            .pop())
    }

    pub async fn fetch_in_range(
        connection: Arc<Mutex<Connection>>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider
                FROM booking, user WHERE booking.user_id = user.id
                AND start_timestamp >= ?1 AND start_timestamp < ?2
                ORDER BY start_timestamp ASC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        stmt.query_map([from.timestamp(), to.timestamp()], map_booking_row)
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect()
    }

    pub async fn fetch_active(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let now = Utc::now().timestamp();
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider
                FROM booking, user WHERE booking.user_id = user.id
                AND start_timestamp <= ?1 AND end_timestamp > ?1
                ORDER BY start_timestamp ASC",
            )
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        stmt.query_map([now], map_booking_row)
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?
            .map(|r| r.map_err(|err| InternalError::new(format!("Failed to map row: {err}"))))
            .collect()
    }
}

fn map_booking_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Booking> {
    Ok(Booking {
        id: row.get(0)?,
        start_time: DateTime::<Utc>::from_timestamp(row.get(1)?, 0).unwrap_or_default(),
        end_time: DateTime::<Utc>::from_timestamp(row.get(2)?, 0).unwrap_or_default(),
        telescope_name: row.get(3)?,
        user_id: row.get(4)?,
        user_name: row.get(5)?,
        user_provider: row.get(6)?,
    })
}

pub async fn consecutive_booking_end(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    telescope_id: &str,
) -> Result<Option<DateTime<Utc>>, InternalError> {
    let bookings = Booking::fetch_for_user(connection, user).await?;
    let now = Utc::now();

    let active = bookings
        .iter()
        .find(|b| b.active_at(&now) && b.telescope_name == telescope_id);
    let Some(active) = active else {
        return Ok(None);
    };

    let mut end_time = active.end_time;
    loop {
        let next = bookings
            .iter()
            .find(|b| b.telescope_name == telescope_id && b.start_time == end_time);
        match next {
            Some(next) => end_time = next.end_time,
            None => break,
        }
    }

    Ok(Some(end_time))
}

pub async fn booking_is_active(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    telescope_id: &str,
) -> Result<bool, InternalError> {
    Ok(Booking::fetch_for_user(connection, user)
        .await?
        .iter()
        .any(|b| b.active_at(&Utc::now()) && b.telescope_name == telescope_id))
}

#[cfg(test)]
mod test {
    use super::*;

    fn create_booking(start_time_ts: i64, end_time_ts: i64) -> Booking {
        Booking {
            id: 0,
            start_time: DateTime::from_timestamp(start_time_ts, 0).unwrap(),
            end_time: DateTime::from_timestamp(end_time_ts, 0).unwrap(),
            telescope_name: String::new(),
            user_id: 0,
            user_name: String::new(),
            user_provider: String::new(),
        }
    }

    #[test]
    fn booking_overlap() {
        let booking1 = create_booking(1, 3);
        let booking2 = create_booking(2, 4);
        assert!(booking1.overlaps(&booking2));
        assert!(booking2.overlaps(&booking1));
    }

    #[test]
    fn booking_no_overlap() {
        let booking1 = create_booking(1, 2);
        let booking2 = create_booking(3, 4);
        assert!(!booking1.overlaps(&booking2));
        assert!(!booking2.overlaps(&booking1));
    }

    #[test]
    fn booking_no_overlap_adjacent() {
        let booking1 = create_booking(1, 2);
        let booking2 = create_booking(2, 3);
        assert!(!booking1.overlaps(&booking2));
        assert!(!booking2.overlaps(&booking1));
    }
}
