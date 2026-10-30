-- 002_connection_legacy_flag.sql
-- S1 Spike 结论落地：允许单个连接开启"兼容老服务器算法"。
-- 默认 0（关闭），只有用户显式打开才追加 SHA-1 系列的 kex/cipher/mac。

ALTER TABLE connections ADD COLUMN legacy_algorithms INTEGER NOT NULL DEFAULT 0;
