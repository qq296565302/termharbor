-- 数据库账号：每台服务器一份，供终端页的数据库面板查询用。
--
-- 为什么单独一张表，而不是往 connections 上加三列：
-- 1. 导出配置时不该把数据库密码带出去（和 SSH 凭据同样的口径）；
-- 2. 没配数据库的连接不需要一个永远为空的端口列。
--
-- password_blob 存的是密文（`[ver:1][nonce:12][ct][tag:16]`），和 SSH 凭据同一套
-- cipher；这一层不认识明文，加解密在命令层做。
CREATE TABLE IF NOT EXISTS db_accounts (
    connection_id TEXT PRIMARY KEY,
    username      TEXT NOT NULL,
    password_blob BLOB    NOT NULL,
    port          INTEGER NOT NULL,
    database_name TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
