-- 本地端口转发（ssh -L）规则。
--
-- 绑定地址固定 127.0.0.1，不进库：把内网服务暴露到局域网是另一回事，
-- 需要单独的开关与警告，不在这一版里偷偷放开。
CREATE TABLE tunnels (
    id            TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL,
    local_port    INTEGER NOT NULL,
    target_host   TEXT NOT NULL,
    target_port   INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

-- 同一条连接上两条规则抢同一个本地端口没有意义，落库时就挡住；
-- 不同连接之间可能撞（运行时 bind 会再拦一次，那时才报"端口被占用"）。
CREATE UNIQUE INDEX idx_tunnels_connection_port ON tunnels (connection_id, local_port);
CREATE INDEX idx_tunnels_connection ON tunnels (connection_id);
