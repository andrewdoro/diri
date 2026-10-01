-- Activation funnel (diri/TELEMETRY.md, Activation). One row per install and
-- milestone: the client records each `activation.*` event once per install,
-- and INSERT OR IGNORE keeps the first copy if a batch is ever re-sent. At
-- most six rows per install, written only by the batches that carry one.
CREATE TABLE milestones (
  install     TEXT NOT NULL,
  step        TEXT NOT NULL,             -- first_launch | agent_ready | first_session | second_session | first_helper | returned
  t           INTEGER NOT NULL,          -- record time, client clock
  preexisting INTEGER NOT NULL,          -- 1: the install used Diri before activation tracking
  since_s     INTEGER,                   -- seconds from first launch
  source      TEXT,                      -- agent_ready: onboarding_install | preexisting | manual
  agent       TEXT,                      -- agent id, where the step has one
  app_version TEXT,
  received_at INTEGER NOT NULL,          -- server clock, for retention
  PRIMARY KEY (install, step)
) WITHOUT ROWID;
-- The funnel's cohort scan: first launches in a time window.
CREATE INDEX milestones_step_t ON milestones (step, t);
CREATE INDEX milestones_received ON milestones (received_at);
