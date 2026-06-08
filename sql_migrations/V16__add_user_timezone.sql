-- Per-user preferred IANA timezone (e.g. 'Europe/Stockholm') for displaying
-- dates and times. NULL means "not chosen yet" — the UI auto-detects the
-- browser timezone on first login and treats NULL as UTC until then.
ALTER TABLE user ADD COLUMN timezone TEXT;
