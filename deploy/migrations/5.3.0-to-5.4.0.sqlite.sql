-- portal SQLite migration, schema of 5.3.0 -> 5.4.0 (ADDITIVE: it keeps every row and drops nothing).
-- 5.4.0 adds the description and the own icon of an external service: three new columns of the table external_services.
-- Stop the portal and copy the database file first. Then run: sqlite3 -bail portal.db < this-file
-- SQLite has no ADD COLUMN IF NOT EXISTS, so run this file one time only. On a second run, SQLite stops at the first
-- statement with "duplicate column name", and the transaction rolls back. A file that 5.4.0 made has the columns already.
-- The checks of the columns are the checks of a new 5.4.0 database.

BEGIN;
ALTER TABLE external_services
  ADD COLUMN description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 200);
ALTER TABLE external_services
  ADD COLUMN icon_type TEXT CHECK (icon_type IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/svg+xml'));
ALTER TABLE external_services
  ADD COLUMN icon BLOB CHECK (icon IS NULL OR (icon_type IS NOT NULL AND length(icon) BETWEEN 1 AND 32768));
COMMIT;
