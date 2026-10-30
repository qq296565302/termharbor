-- 001_init.sql — MVP 初始 schema
-- 只放 DDL，PRAGMA（journal_mode / foreign_keys）在 storage/db.rs 里单独设置，
-- 避免 execute_batch 碰到会返回结果集的语句。

CREATE TABLE IF NOT EXISTS schema_migrations (
  version    INTEGER PRIMARY KEY,
  name       TEXT NOT NULL,
  applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS groups (
  id   TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  sort INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS connections (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  host          TEXT NOT NULL,
  port          INTEGER NOT NULL DEFAULT 22,
  username      TEXT NOT NULL,
  auth_type     TEXT NOT NULL CHECK (auth_type IN ('password', 'key')),
  -- 密文布局：[version:1][nonce:12][ciphertext][tag:16]，见 crypto::cipher
  secret_blob   BLOB,
  key_path      TEXT,
  key_pass_blob BLOB,
  group_id      TEXT REFERENCES groups (id) ON DELETE SET NULL,
  last_used_at  INTEGER,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_connections_name ON connections (name);
CREATE INDEX IF NOT EXISTS idx_connections_host ON connections (host);

CREATE TABLE IF NOT EXISTS known_hosts (
  host_key    TEXT PRIMARY KEY, -- "host:port"
  algo        TEXT NOT NULL,
  fingerprint TEXT NOT NULL,    -- SHA256:base64(nopad)，与 OpenSSH 一致
  added_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- P1 预留：M0 建表，M6 起填数据
CREATE TABLE IF NOT EXISTS snippets (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  command     TEXT NOT NULL,
  description TEXT,
  grp         TEXT,
  risk_level  TEXT NOT NULL CHECK (risk_level IN ('readonly', 'dangerous', 'critical')),
  variables   TEXT,
  is_builtin  INTEGER NOT NULL DEFAULT 0,
  builtin_key TEXT UNIQUE,
  sort        INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
);
