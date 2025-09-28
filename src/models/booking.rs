use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::user::User;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Booking {
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

    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: User,
        telescope_id: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Booking, InternalError> {
        let conn = connection.lock().await;
        conn.execute(
            "insert into booking (user_id, telescope_id, start_timestamp, end_timestamp)
                 values ((?1), (?2), (?3), (?4))",
            (&user.id, &telescope_id, start.timestamp(), end.timestamp()),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert user in db: {err}")))?;
        Ok(Booking {
            start_time: start,
            end_time: end,
            telescope_name: telescope_id,
            user_name: user.name,
            user_provider: user.provider,
        })
    }

    async fn fetch(
        connection: Arc<Mutex<Connection>>,
        where_cond: Option<String>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let query = String::from(
            "select start_timestamp, end_timestamp, telescope_id, username, provider
                        from booking, user
                        where booking.user_id = user.id",
        ) + &where_cond.map_or(String::new(), |c| format!(" and {c}"));
        let mut stmt = conn
            .prepare(&query)
            .map_err(|err| InternalError::new(format!("Failed to prepare statement: {err}")))?;
        let bookings = stmt
            .query_map([], |row| {
                Ok(Booking {
                    start_time: DateTime::<Utc>::from_timestamp(row.get(0)?, 0).unwrap(),
                    end_time: DateTime::<Utc>::from_timestamp(row.get(1)?, 0).unwrap(),
                    telescope_name: row.get(2)?,
                    user_name: row.get(3)?,
                    user_provider: row.get(4)?,
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
        Self::fetch(connection, Some(format!("user.id == {}", user.id))).await
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn create_booking(start_time_ts: i64, end_time_ts: i64) -> Booking {
        Booking {
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
