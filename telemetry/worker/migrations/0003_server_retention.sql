-- Server receipt times, independent of untrusted event clocks. Existing index
-- rows cannot be reliably matched to batches; expire them conservatively on
-- the next sweep rather than granting potentially ancient data a new lifetime.
-- DEFAULT 0 also fails closed for writes from an old Worker during deployment.
ALTER TABLE incidents ADD COLUMN received_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN received_at INTEGER NOT NULL DEFAULT 0;
CREATE INDEX incidents_received ON incidents (received_at);
CREATE INDEX sessions_received ON sessions (received_at);
