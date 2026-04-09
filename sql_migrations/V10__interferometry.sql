CREATE TABLE interferometry_session (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES user(id),
    start_time INTEGER NOT NULL,
    end_time INTEGER,
    telescope_a TEXT NOT NULL,
    telescope_b TEXT NOT NULL,
    coordinate_system TEXT NOT NULL,
    target_x REAL NOT NULL,
    target_y REAL NOT NULL,
    center_freq_hz REAL NOT NULL,
    bandwidth_hz REAL NOT NULL
);

CREATE TABLE interferometry_visibility (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES interferometry_session(id) ON DELETE CASCADE,
    time INTEGER NOT NULL,
    mean_amplitude REAL NOT NULL,
    mean_phase_deg REAL NOT NULL,
    delay_ns REAL NOT NULL,
    amplitudes_json TEXT NOT NULL,
    phases_json TEXT NOT NULL,
    frequencies_json TEXT NOT NULL
);
