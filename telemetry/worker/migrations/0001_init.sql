-- Index over the raw batches in R2. R2 holds the truth (every record, as
-- uploaded); D1 only answers "which batches / incidents / sessions belong to
-- whom and when". Every index here costs one extra D1 row write per insert,
-- so each one is justified by a query in src/admin.ts.

CREATE TABLE installs (
  install     TEXT PRIMARY KEY,          -- lowercase UUID
  support_id  TEXT NOT NULL,             -- D-XXXXXXXX
  name        TEXT,                      -- user-editable, may be null
  app_version TEXT,
  build       TEXT,
  channel     TEXT,
  os          TEXT,
  os_version  TEXT,
  arch        TEXT,
  first_seen  INTEGER NOT NULL,          -- ms, server receive time
  last_seen   INTEGER NOT NULL
);
-- No secondary indexes: one row per Mac, so `who` scans it, and an index
-- would cost a row write on every batch's upsert.

CREATE TABLE batches (
  id          INTEGER PRIMARY KEY,
  install     TEXT NOT NULL,
  r2_key      TEXT NOT NULL,
  received_at INTEGER NOT NULL,          -- ms, server clock
  sent_at     INTEGER NOT NULL,          -- ms, client clock (header)
  t_min       INTEGER,                   -- record time range, client clock
  t_max       INTEGER,
  procs       TEXT NOT NULL,             -- comma-separated, e.g. "app,engine"
  lines       INTEGER NOT NULL,          -- records accepted
  bad_lines   INTEGER NOT NULL,          -- records that were not JSON objects
  bytes       INTEGER NOT NULL,          -- gzip body size
  raw_bytes   INTEGER NOT NULL,          -- decompressed size
  app_version TEXT
);
-- Rate limiting (count in the last hour) and the timeline's batch range scan.
CREATE INDEX batches_install_received ON batches (install, received_at);

CREATE TABLE incidents (
  id          INTEGER PRIMARY KEY,
  install     TEXT NOT NULL,
  t           INTEGER NOT NULL,
  seq         INTEGER,
  proc        TEXT,
  pid         INTEGER,
  kind        TEXT NOT NULL,
  sev         TEXT NOT NULL,             -- error | incident
  session     TEXT,
  conv        TEXT,
  agent       TEXT,
  code        TEXT,
  signature   TEXT NOT NULL,
  app_version TEXT,
  fields      TEXT NOT NULL              -- the record's f, JSON, <= 2 KiB
);
CREATE INDEX incidents_install_t ON incidents (install, t);
CREATE INDEX incidents_t ON incidents (t);

-- One row per (session, conversation) pair; conv = '' carries the span of
-- records that named the session without a conversation.
CREATE TABLE sessions (
  install TEXT NOT NULL,
  session TEXT NOT NULL,
  conv    TEXT NOT NULL DEFAULT '',
  agent   TEXT,
  first_t INTEGER NOT NULL,
  last_t  INTEGER NOT NULL,
  PRIMARY KEY (install, session, conv)
) WITHOUT ROWID;
CREATE INDEX sessions_session ON sessions (session);
-- Partial: most rows have no conversation. Queries must say conv != ''.
CREATE INDEX sessions_conv ON sessions (conv) WHERE conv != '';
