-- Agent 运行时第二阶段：记忆分层 + 主动提醒
--
-- 设计参照 2026-11-20《Linger 基座调研与架构借鉴》：
-- 1. 记忆分层：fact（长期事实）/ preference（偏好线索）/ rule（家规）。
--    重复出现的偏好被巩固（reinforced_count），第二次自动升级为家规。
-- 2. 主动提醒：三态决策（告知 / 跟进 / 沉默）+ 闸门流水。
--    agent_reminders 是事项本身（同家庭同去重键唯一），
--    agent_proactive_log 是每次扫描的判定流水（用于「同事项同状态每天只判一次」）。
--
-- 这些表与 000004 迁移中的 agent_* 表一样，不参与 P2P 同步。

-- ===== 记忆分层 =====

ALTER TABLE agent_memories
    ADD COLUMN IF NOT EXISTS kind VARCHAR(16) NOT NULL DEFAULT 'fact'
        CHECK (kind IN ('fact', 'preference', 'rule')),
    ADD COLUMN IF NOT EXISTS importance SMALLINT NOT NULL DEFAULT 3,
    ADD COLUMN IF NOT EXISTS reinforced_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_reinforced_at TIMESTAMPTZ;

-- 分层回灌：家规优先、偏好次之、事实垫后，同层按重要度与新度排序。
CREATE INDEX IF NOT EXISTS idx_agent_memories_recall
    ON agent_memories (family_id, kind, importance DESC, created_at DESC);

-- 同一家庭内，内容相同的记忆只应有一条 —— 再次「记住」是**确认**，
-- 走强化路径（reinforced_count +1，偏好升级为家规），而不是插一条重复行。
-- 先清掉可能已存在的重复行（保留最早一条并合并强化次数），再建唯一索引。
WITH dupes AS (
    SELECT id,
           ROW_NUMBER() OVER (PARTITION BY family_id, content ORDER BY created_at ASC, id ASC) AS rn
    FROM agent_memories
)
DELETE FROM agent_memories d
USING dupes x
WHERE d.id = x.id AND x.rn > 1;

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_memories_family_content
    ON agent_memories (family_id, content);

-- ===== 主动提醒 =====

CREATE TABLE IF NOT EXISTS agent_reminders (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    -- 信号类型：expiring_thing / upcoming_event / overdue_task
    kind VARCHAR(32) NOT NULL,
    -- 机读事项标识（物品/日程/待办的 ID），与 state 共同构成去重维度
    subject UUID NOT NULL,
    -- 事项状态指纹：如到期日 2026-11-25。同一事项状态变化时（改期/续期）应重新判定
    state VARCHAR(64) NOT NULL DEFAULT '',
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    -- 建议用户执行的唯一一个动作
    action TEXT NOT NULL DEFAULT '',
    urgency VARCHAR(8) NOT NULL DEFAULT 'normal'
        CHECK (urgency IN ('normal', 'urgent')),
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'told', 'snoozed', 'dismissed')),
    dedupe_key TEXT NOT NULL,
    fire_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 同一家庭、同一去重键只保留一条提醒；重复扫描走更新而非插入
    UNIQUE (family_id, dedupe_key)
);

-- 主动判定流水：每次扫描对每个候选信号记一行，闸门据此去重与限流。
CREATE TABLE IF NOT EXISTS agent_proactive_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    reminder_id UUID REFERENCES agent_reminders(id) ON DELETE SET NULL,
    dedupe_key TEXT NOT NULL,
    -- tell / follow_up / silent
    decision VARCHAR(16) NOT NULL CHECK (decision IN ('tell', 'follow_up', 'silent')),
    reason VARCHAR(48) NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_reminders_family_status
    ON agent_reminders (family_id, status, fire_at);
CREATE INDEX IF NOT EXISTS idx_agent_proactive_log_today
    ON agent_proactive_log (family_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_proactive_log_dedupe
    ON agent_proactive_log (family_id, dedupe_key, created_at DESC);
