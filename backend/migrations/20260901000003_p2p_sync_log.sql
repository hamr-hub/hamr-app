-- P2P 同步日志：last-write-wins 合并的幂等去重台账
--
-- 每条被真正应用的 SyncRecord 都会在这里留一行。handle_incoming_sync 先查
-- sync_id 是否已存在：存在 → 直接 ack 不再写库（幂等去重，防 gossipsub 重播 /
-- HTTP 重试导致重复落库）；不存在 → 比较 timestamp 决定是否覆盖。
--
-- 命名说明：字段语义上是「表名」，但 `table` 是 SQL 保留字，裸用会导致语法错误，
-- 必须写成 "table" 才能建列。为免每条查询都要加引号（极易漏、漏了就报错），
-- 这里物理列名取 table_name。
CREATE TABLE IF NOT EXISTS sync_log (
    -- 发送方生成的全局唯一同步 ID（UUID v4 字符串），幂等键
    sync_id     TEXT PRIMARY KEY,
    -- 被同步的业务表名（people/events/tasks/things/spaces 白名单内）
    table_name  TEXT NOT NULL,
    -- 业务行主键（UUID 字符串）
    primary_key TEXT NOT NULL,
    -- 本地应用该条同步的时间
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 「某张表某一行最近被同步过什么」的排查/回溯路径
CREATE INDEX IF NOT EXISTS idx_sync_log_table_pk ON sync_log(table_name, primary_key);
-- 按时间清理历史台账（保留窗口外的行可安全删除，幂等窗口即保留窗口）
CREATE INDEX IF NOT EXISTS idx_sync_log_applied_at ON sync_log(applied_at);
