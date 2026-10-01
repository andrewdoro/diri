-- Spend guard. R2 is the only resource that bills past its free tier (the
-- Workers Free plan simply fails requests over its limits), so the Worker
-- counts its own R2 usage here and refuses work before crossing the free
-- tier. One row per (period, metric):
--   ('2026-10', 'puts')       R2 Class A writes this month
--   ('2026-10', 'gets')       R2 Class B reads this month (admin batch fetch)
--   ('2026-10-01', 'bytes')   bytes written to R2 that day; the sum over the
--                             retention window is what R2 is storing
CREATE TABLE budget (
  period TEXT NOT NULL,
  metric TEXT NOT NULL,
  value  INTEGER NOT NULL,
  PRIMARY KEY (period, metric)
) WITHOUT ROWID;
