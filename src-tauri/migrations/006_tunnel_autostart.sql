-- 规则是否在应用启动时自动拉起。
--
-- 默认 0：临时调试用的规则不该每次开机都被建起来；长期要挂的那条
-- （比如内网数据库）由用户在规则上单独打开开关。
ALTER TABLE tunnels ADD COLUMN auto_start INTEGER NOT NULL DEFAULT 0;

-- 启动时要按「开着的规则」一次性取出来，单列索引足够。
CREATE INDEX idx_tunnels_auto_start ON tunnels (auto_start);
