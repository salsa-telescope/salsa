-- Per-user preferred UI language as an ISO 639-1 code (e.g. 'sv'). NULL
-- means "no preference" — the language cookie or the browser's
-- Accept-Language header decides instead.
ALTER TABLE user ADD COLUMN language TEXT;
