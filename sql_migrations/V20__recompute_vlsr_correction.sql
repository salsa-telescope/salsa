-- Data fixups that need Rust code (here: coordinate maths SQLite can't do).
-- A migration inserts a row naming the fixup; `database::apply_migrations`
-- runs the matching code once and deletes the row in the same transaction.
CREATE TABLE pending_data_fixup (name TEXT PRIMARY KEY NOT NULL);

-- Up to v1.5.28 the VLSR correction had the wrong sign on the Earth's orbital
-- term for targets with R.A. between 6h and 18h (galactic l ≈ 186°–6° at
-- b = 0), an error of up to ~60 km/s. Recompute the stored value for every
-- galactic observation from its target and start time.
INSERT INTO pending_data_fixup (name) VALUES ('recompute_vlsr_correction');
