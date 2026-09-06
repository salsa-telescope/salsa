use std::sync::Arc;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
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
    pub description: Option<String>,
    pub country: Option<String>,
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
        let rows_deleted = if user.is_admin {
            conn.execute("DELETE FROM booking WHERE id = (?1)", (&self.id,))
        } else {
            conn.execute(
                "DELETE FROM booking WHERE id = (?1) AND user_id = (?2)",
                (&self.id, &user.id),
            )
        }
        .map_err(|err| InternalError::new(format!("Failed to delete booking from db: {err}")))?;
        if rows_deleted >= 2 {
            return Err(InternalError::new(format!(
                "Unexpected number of rows deleted: {rows_deleted}"
            )));
        }
        Ok(rows_deleted > 0)
    }

    /// Insert a booking iff no overlapping booking exists for the same telescope.
    /// Returns `true` if the row was inserted, `false` if a conflicting booking
    /// already exists. The conflict check and insert run as a single SQL
    /// statement, so this is safe against concurrent create requests.
    pub async fn create(
        connection: Arc<Mutex<Connection>>,
        user: User,
        telescope_id: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        description: Option<String>,
        country: Option<String>,
    ) -> Result<bool, InternalError> {
        let conn = connection.lock().await;
        let rows = conn.execute(
            "INSERT INTO booking (user_id, telescope_id, start_timestamp, end_timestamp, description, country)
                 SELECT (?1), (?2), (?3), (?4), (?5), (?6)
                 WHERE NOT EXISTS (
                     SELECT 1 FROM booking
                     WHERE telescope_id = (?2)
                       AND start_timestamp < (?4)
                       AND end_timestamp > (?3)
                 )",
            (&user.id, &telescope_id, start.timestamp(), end.timestamp(), &description, &country),
        )
        .map_err(|err| InternalError::new(format!("Failed to insert booking in db: {err}")))?;
        Ok(rows > 0)
    }

    pub async fn fetch_all(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider, description, country
                FROM booking, user WHERE booking.user_id = user.id
                ORDER BY start_timestamp ASC",
            )?;
        stmt.query_map([], map_booking_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(InternalError::from)
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
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider, description, country
                FROM booking, user WHERE booking.user_id = user.id AND user.id = ?1
                ORDER BY start_timestamp ASC",
            )?;
        stmt.query_map([user_id], map_booking_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(InternalError::from)
    }

    pub async fn fetch_one(
        connection: Arc<Mutex<Connection>>,
        id: i64,
    ) -> Result<Option<Booking>, InternalError> {
        let conn = connection.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider, description, country
                FROM booking, user WHERE booking.user_id = user.id AND booking.id = ?1
                ORDER BY start_timestamp ASC",
            )?;
        Ok(stmt
            .query_map([id], map_booking_row)?
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
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider, description, country
                FROM booking, user WHERE booking.user_id = user.id
                AND start_timestamp >= ?1 AND start_timestamp < ?2
                ORDER BY start_timestamp ASC",
            )?;
        stmt.query_map([from.timestamp(), to.timestamp()], map_booking_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(InternalError::from)
    }

    pub async fn fetch_active(
        connection: Arc<Mutex<Connection>>,
    ) -> Result<Vec<Booking>, InternalError> {
        let conn = connection.lock().await;
        let now = Utc::now().timestamp();
        let mut stmt = conn
            .prepare(
                "SELECT booking.id, start_timestamp, end_timestamp, telescope_id, user.id, username, provider, description, country
                FROM booking, user WHERE booking.user_id = user.id
                AND start_timestamp <= ?1 AND end_timestamp > ?1
                ORDER BY start_timestamp ASC",
            )?;
        stmt.query_map([now], map_booking_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(InternalError::from)
    }
}

/// A run of back-to-back bookings on one telescope, presented as a single
/// item. Users book long sessions an hour at a time, so an afternoon of
/// observing is a dozen rows in the database but one thing to the person
/// who booked it — this is that one thing, used by the "upcoming
/// bookings" list and the .ics export.
#[derive(Debug, Clone, PartialEq)]
pub struct BookingGroup {
    /// Ids of the underlying hourly bookings, in chronological order.
    pub ids: Vec<i64>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub telescope_name: String,
    pub user_name: String,
    pub description: Option<String>,
}

impl BookingGroup {
    pub fn active_at(&self, instant: &DateTime<Utc>) -> bool {
        *instant > self.start_time && *instant < self.end_time
    }

    /// Number of hourly slots in the run, which is also how many slots of
    /// the user's quota it spends.
    pub fn slot_count(&self) -> usize {
        self.ids.len()
    }

    pub fn hours(&self) -> i64 {
        (self.end_time - self.start_time).num_hours()
    }

    /// True when the run ends on a later local date than it starts, so the
    /// display has to spell out the end date instead of just the time.
    pub fn crosses_local_day(&self, tz: &Tz) -> bool {
        self.start_time.with_timezone(tz).date_naive()
            != self.end_time.with_timezone(tz).date_naive()
    }

    /// Booking ids as "12,13,14", for a `data-` attribute the cancel
    /// dialog reads back.
    pub fn id_list(&self) -> String {
        self.ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Collapse each run of touching bookings into one [`BookingGroup`].
///
/// Two bookings merge when they are on the same telescope, one ends
/// exactly where the next begins, and they carry the same description —
/// so a block booked in one go (the dialog writes one description to
/// every slot) stays one item, while two sessions that merely abut with
/// different descriptions stay apart. `bookings` must be sorted by start
/// time, as every `fetch_*` query returns them.
pub fn group_adjacent(bookings: &[Booking]) -> Vec<BookingGroup> {
    let mut groups: Vec<BookingGroup> = Vec::new();
    for booking in bookings {
        // Bookings for different telescopes interleave in the sorted
        // input, so look for the run to extend rather than only checking
        // the group added last.
        let extendable = groups.iter_mut().find(|group| {
            group.telescope_name == booking.telescope_name
                && group.user_name == booking.user_name
                && group.description == booking.description
                && group.end_time == booking.start_time
        });
        match extendable {
            Some(group) => {
                group.end_time = booking.end_time;
                group.ids.push(booking.id);
            }
            None => groups.push(BookingGroup {
                ids: vec![booking.id],
                start_time: booking.start_time,
                end_time: booking.end_time,
                telescope_name: booking.telescope_name.clone(),
                user_name: booking.user_name.clone(),
                description: booking.description.clone(),
            }),
        }
    }
    groups
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
        description: row.get(7)?,
        country: row.get(8)?,
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

/// Authorisation gate for the observe page and its sub-handlers: returns
/// true if the user has either a real booking active right now or an
/// active guest session. Guest sessions live in their own table (see
/// `models::guest`) precisely so the booking calendar stays untouched —
/// this helper is where the two paths meet.
pub async fn is_authorized_for_telescope(
    connection: Arc<Mutex<Connection>>,
    user: &User,
    telescope_id: &str,
) -> Result<bool, InternalError> {
    if booking_is_active(connection.clone(), user, telescope_id).await? {
        return Ok(true);
    }
    crate::models::guest::guest_is_active(connection, user, telescope_id).await
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
            description: None,
            country: None,
        }
    }

    const HOUR: i64 = 3600;

    /// One hourly booking, as the calendar creates them.
    fn create_slot(id: i64, hour: i64, telescope: &str, description: Option<&str>) -> Booking {
        Booking {
            id,
            start_time: DateTime::from_timestamp(hour * HOUR, 0).unwrap(),
            end_time: DateTime::from_timestamp((hour + 1) * HOUR, 0).unwrap(),
            telescope_name: telescope.to_string(),
            user_id: 0,
            user_name: String::new(),
            user_provider: String::new(),
            description: description.map(str::to_string),
            country: None,
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

    #[test]
    fn adjacent_bookings_group_into_one() {
        let bookings = [
            create_slot(1, 10, "vale", None),
            create_slot(2, 11, "vale", None),
            create_slot(3, 12, "vale", None),
        ];
        let groups = group_adjacent(&bookings);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].ids, vec![1, 2, 3]);
        assert_eq!(groups[0].start_time, bookings[0].start_time);
        assert_eq!(groups[0].end_time, bookings[2].end_time);
        assert_eq!(groups[0].hours(), 3);
        assert_eq!(groups[0].slot_count(), 3);
        assert_eq!(groups[0].id_list(), "1,2,3");
    }

    #[test]
    fn gap_splits_groups() {
        let bookings = [
            create_slot(1, 10, "vale", None),
            create_slot(2, 12, "vale", None),
        ];
        let groups = group_adjacent(&bookings);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].ids, vec![1]);
        assert_eq!(groups[1].ids, vec![2]);
    }

    #[test]
    fn different_telescopes_do_not_group() {
        let bookings = [
            create_slot(1, 10, "vale", None),
            create_slot(2, 11, "brage", None),
        ];
        let groups = group_adjacent(&bookings);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn different_descriptions_do_not_group() {
        let bookings = [
            create_slot(1, 10, "vale", Some("Cas A")),
            create_slot(2, 11, "vale", Some("Cygnus")),
        ];
        let groups = group_adjacent(&bookings);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].description.as_deref(), Some("Cas A"));
        assert_eq!(groups[1].description.as_deref(), Some("Cygnus"));
    }

    /// Bookings on two telescopes interleave in the start-time-sorted
    /// input, so a run must still be found past the other telescope's
    /// slots.
    #[test]
    fn interleaved_telescopes_still_group() {
        let bookings = [
            create_slot(1, 10, "vale", None),
            create_slot(2, 10, "brage", None),
            create_slot(3, 11, "vale", None),
            create_slot(4, 11, "brage", None),
        ];
        let groups = group_adjacent(&bookings);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].telescope_name, "vale");
        assert_eq!(groups[0].ids, vec![1, 3]);
        assert_eq!(groups[1].telescope_name, "brage");
        assert_eq!(groups[1].ids, vec![2, 4]);
    }

    #[test]
    fn group_is_active_inside_the_whole_run() {
        let bookings = [
            create_slot(1, 10, "vale", None),
            create_slot(2, 11, "vale", None),
        ];
        let groups = group_adjacent(&bookings);
        let inside_second_hour = DateTime::from_timestamp(11 * HOUR + 60, 0).unwrap();
        let after = DateTime::from_timestamp(12 * HOUR + 60, 0).unwrap();
        assert!(groups[0].active_at(&inside_second_hour));
        assert!(!groups[0].active_at(&after));
    }

    #[test]
    fn no_bookings_no_groups() {
        assert!(group_adjacent(&[]).is_empty());
    }
}
