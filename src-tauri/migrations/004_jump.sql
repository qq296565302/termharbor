-- 跳板机：值指向另一条连接的 id，空 = 直连。
--
-- 刻意不加外键约束：删连接时由 connections::delete 显式把引用清空
-- （多一句可见的 UPDATE，行为可读、可测；隐式 SET NULL 在这里反而不好排查）。
-- 这里只存一层：多跳靠「跳板自己也有跳板」自然串成链。
ALTER TABLE connections ADD COLUMN jump_connection_id TEXT;
