-- Indexes for the lookups that run on a timer or on every request.
--
-- Until now the only indexed table was guest_session (V15). Everything else
-- was scanned, which was invisible while the tables were small but scales
-- with use rather than with anything the user does.

-- The hot one: session_middleware resolves this token on every single
-- request, and the observe page polls once a second. UNIQUE because tokens
-- are generated unique already — the constraint documents that and lets
-- SQLite stop at the first match.
CREATE UNIQUE INDEX idx_session_token ON session(token);

-- Looked up once per completed OAuth2 login.
CREATE INDEX idx_pending_oauth2_csrf ON pending_oauth2(csrf_token);

-- Visibility rows are written throughout an interferometry run and read back
-- by session; also what the ON DELETE CASCADE sweep walks when a session is
-- deleted.
CREATE INDEX idx_visibility_session ON interferometry_visibility(session_id);

-- Matches both the archive list page (WHERE user_id ORDER BY start_time DESC,
-- which this serves without a sort step) and its COUNT(*).
CREATE INDEX idx_observation_user_start ON observation(user_id, start_time);

-- Booking::fetch_active runs once a second from booking_monitor, and again
-- every five seconds from guest_monitor.
CREATE INDEX idx_booking_telescope_time ON booking(telescope_id, start_timestamp);
