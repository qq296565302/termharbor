-- 命令历史 / 路径历史。刻意不设条数上限（对照 FinalShell「无限制历史记录」）：
-- 相同 value + connection_id 只留一行，靠 use_count / ts_ms 更新，所以长期用也不会膨胀成重复行。
CREATE TABLE IF NOT EXISTS history (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL,          -- 'command' | 'path'
  value          TEXT NOT NULL,
  connection_id  TEXT NOT NULL DEFAULT '',
  ts_ms          INTEGER NOT NULL,
  use_count      INTEGER NOT NULL DEFAULT 1
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_history_unique
  ON history(kind, connection_id, value);

CREATE INDEX IF NOT EXISTS idx_history_kind_ts
  ON history(kind, ts_ms DESC);
