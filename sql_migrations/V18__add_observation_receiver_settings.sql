-- Receiver settings the observation was taken with. Until now only the target
-- and integration time were kept, so an archived spectrum could not be told
-- apart from one taken at a different gain or in a different observation mode.
-- Diagnosing receiver compression on Vale (issue #317) depended entirely on
-- knowing the gain, which was recorded nowhere.
--
-- Nullable throughout: rows written before this migration have no settings to
-- recover. Centre frequency and bandwidth are derivable from frequencies_json
-- for old rows, but are stored explicitly from now on so a reader does not
-- have to reconstruct them.
ALTER TABLE observation ADD COLUMN gain_db REAL;
ALTER TABLE observation ADD COLUMN center_freq_hz REAL;
ALTER TABLE observation ADD COLUMN ref_freq_hz REAL;
ALTER TABLE observation ADD COLUMN bandwidth_hz REAL;
ALTER TABLE observation ADD COLUMN spectral_channels INTEGER;
ALTER TABLE observation ADD COLUMN observation_mode TEXT;
ALTER TABLE observation ADD COLUMN rfi_filter INTEGER;
