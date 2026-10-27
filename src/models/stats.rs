//! Raw rows for the public statistics page. Only the columns the page needs
//! are read, so the queries stay light even though the page is public and
//! computed on every request.

use std::sync::Arc;

use chrono::DateTime;
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::error::InternalError;
use crate::models::booking::Booking;

pub struct GuestRow {
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub country: Option<String>,
}

pub struct ObservationRow {
    pub user_id: i64,
    pub start_time: i64,
}

pub struct RawStats {
    /// Bookings starting at or after `since`, all users. The caller drops
    /// admin bookings.
    pub bookings: Vec<Booking>,
    pub guests: Vec<GuestRow>,
    pub observations: Vec<ObservationRow>,
    /// Ids of registered accounts: guests and deleted (anonymised)
    /// accounts left out.
    pub registered_user_ids: Vec<i64>,
}

/// Everything the statistics page counts, from `since` (unix seconds) on.
/// Read under one lock so the figures come from one consistent snapshot.
pub async fn fetch(
    connection: Arc<Mutex<Connection>>,
    since: i64,
) -> Result<RawStats, InternalError> {
    let conn = connection.lock().await;

    let bookings = conn
        .prepare(
            "SELECT id, user_id, telescope_id, start_timestamp, end_timestamp, country
             FROM booking WHERE start_timestamp >= ?1",
        )?
        .query_map([since], |row| {
            Ok(Booking {
                id: row.get(0)?,
                user_id: row.get(1)?,
                telescope_name: row.get(2)?,
                start_time: DateTime::from_timestamp(row.get(3)?, 0).unwrap_or_default(),
                end_time: DateTime::from_timestamp(row.get(4)?, 0).unwrap_or_default(),
                user_name: String::new(),
                user_provider: String::new(),
                description: None,
                country: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let guests = conn
        .prepare("SELECT started_at, ended_at, country FROM guest_session WHERE started_at >= ?1")?
        .query_map([since], |row| {
            Ok(GuestRow {
                started_at: row.get(0)?,
                ended_at: row.get(1)?,
                country: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let observations = conn
        .prepare("SELECT user_id, start_time FROM observation WHERE start_time >= ?1")?
        .query_map([since], |row| {
            Ok(ObservationRow {
                user_id: row.get(0)?,
                start_time: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Deleting an account blanks its provider.
    let registered_user_ids = conn
        .prepare("SELECT id FROM user WHERE COALESCE(provider, '') NOT IN ('guest', '')")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(RawStats {
        bookings,
        guests,
        observations,
        registered_user_ids,
    })
}
