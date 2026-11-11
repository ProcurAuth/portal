-- portal PostgreSQL migration, schema of 5.3.0 -> 5.4.0 (ADDITIVE and IDEMPOTENT: it keeps every row and drops nothing).
-- 5.4.0 adds the description and the own icon of an external service: three new columns of the table external_services.
-- Run it with the portal stopped, as the database owner: psql -v ON_ERROR_STOP=1 -f this-file. Take a pg_dump first.
-- A second run changes nothing: each column is added only when it is missing. A database that 5.4.0 made has the columns
-- already, and the file then does nothing.
-- The checks of the columns are the checks of a new 5.4.0 database, so the database refuses a value that the rules of the
-- portal refuse: a description of more than 200 characters, an image of more than 32 KiB, a type that is not an image
-- type, and an image without its type.

BEGIN;
ALTER TABLE external_services
  ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT ''
    CHECK (char_length(description) <= 200);
ALTER TABLE external_services
  ADD COLUMN IF NOT EXISTS icon_type TEXT
    CHECK (icon_type IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/svg+xml'));
ALTER TABLE external_services
  ADD COLUMN IF NOT EXISTS icon BYTEA
    CHECK (icon IS NULL OR (icon_type IS NOT NULL AND octet_length(icon) BETWEEN 1 AND 32768));

-- The result: the three columns exist. If not, the transaction stops and rolls back.
DO $$
BEGIN
  IF (SELECT COUNT(*) FROM information_schema.columns
       WHERE table_schema = current_schema()
         AND table_name = 'external_services'
         AND column_name IN ('description', 'icon_type', 'icon')) <> 3 THEN
    RAISE EXCEPTION 'external_services lacks a column of 5.4.0: description, icon_type or icon';
  END IF;
END
$$;
COMMIT;
