CREATE TABLE observation (
    "id" INTEGER PRIMARY KEY,
    "user_id" INTEGER NOT NULL,
    "telescope_id" TEXT NOT NULL,
    "start_time" INTEGER NOT NULL,
    "coordinate_system" TEXT NOT NULL,
    "target_x" REAL NOT NULL,
    "target_y" REAL NOT NULL,
    "integration_time_secs" REAL NOT NULL,
    "frequencies_json" TEXT NOT NULL,
    "amplitudes_json" TEXT NOT NULL,
    CONSTRAINT fk_user_id FOREIGN KEY ("user_id") REFERENCES user ("id") ON DELETE CASCADE
);
