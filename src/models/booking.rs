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

    async fn fetch(
        connection: Arc<Mutex<Connection>>,
        where_cond: Option<String>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let query = String::from(
            "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, username, provider
                FROM booking, user
                WHERE booking.user_id = user.id",
        ) + &where_cond.map_or(String::new(), |c| format!(" and {c}"))
            + " ORDER BY start_timestamp ASC";
        let mut stmt = conn
            .prepare(&query)
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let bookings = stmt
            .query_map([], |row| {
                Ok(Booking {
                    id: row.get(0)?,
                    start_time: DateTime::<Utc>::from_timestamp(row.get(1)?, 0).unwrap(),
                    end_time: DateTime::<Utc>::from_timestamp(row.get(2)?, 0).unwrap(),
                    telescope_name: row.get(3)?,
                    user_name: row.get(4)?,
                    user_provider: row.get(5)?,
                })
            })
            .map_err(|err| InternalError::new(format!("Failed to query_map: {err}")))?;

        let mut res = Vec::new();
        for booking in bookings {
            match booking {
                Ok(booking) => res.push(booking),
                Err(err) => {
                    return Err(InternalError::new(format!("Failed to map row: {err}")));
                }
            }
        }
        Ok(res)
    }

    pub async fn fetch_all(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<Booking>, InternalError> {
        Self::fetch(connection, None).await
    }

    pub async fn fetch_for_user(
        connection: Arc<Mutex<Connection>>,
        user: User,
    ) -> Result<Vec<Booking>, InternalError> {
        // FIXME: Even though user.id can be trusted not to be a random string,
        // use a prepared statement to avoid injection.
        Self::fetch(connection, Some(format!("user.id == {}", user.id))).await
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
    ) -> Result<Option<Booking>, InternalError> {
        // FIXME: Even though id can be trusted not to be a random string,
        // use a prepared statement to avoid injection.
        let booking = Self::fetch(connection, Some(format!("booking.id == {}", id)))
            .await?
            .pop();
        Ok(booking)
    }
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
