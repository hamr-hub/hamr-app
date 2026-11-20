-- 家庭版 Agent 运行时：家庭 Wiki、技能包、写操作审批、成员维度会话
--
-- 设计参照 2026-11-20《家庭版 Agent 架构设计（仿 Linger）》：
--
-- 1. 家庭 Wiki（Linger 的 personal wiki 对应物）
--    每个家庭实体（成员/物品/空间/日程/待办）一页，页内分节，
--    每条事实都带来源引用，改动进 append-only 变更日志。
--    关键约束：**页面由应用侧从五维数据确定性重建，模型只能读取，不能写**。
--
-- 2. 技能包（Linger 的 Skill 对应物）
--    版本化、不可变；文件在磁盘，DB 只存每个家庭的安装与启用状态。
--
-- 3. 写操作审批（Linger 的 approvals 对应物）
--    模型只能「提议」，落库必须经用户显式确认；operation_id 幂等。
--
-- 4. 成员维度
--    会话可归属某个家庭成员（member_id），为空表示家庭公共会话。
--
-- 与 000004 / 000005 迁移中的 agent_* 表一样，不参与 P2P 同步。

-- ===== 成员维度会话 =====

ALTER TABLE agent_sessions
    ADD COLUMN IF NOT EXISTS member_id UUID;

CREATE INDEX IF NOT EXISTS idx_agent_sessions_member
    ON agent_sessions (family_id, member_id, updated_at DESC);

-- ===== 家庭 Wiki =====

-- 实体页：一个家庭实体一页
CREATE TABLE IF NOT EXISTS agent_wiki_pages (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    -- member / thing / space / event / task / family
    entity_type VARCHAR(32) NOT NULL,
    -- 对应五维表的行 ID；家庭总览页固定为 'family'
    entity_key VARCHAR(64) NOT NULL,
    title VARCHAR(200) NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (family_id, entity_type, entity_key)
);

-- 页内分节（一个实体可以有「基本信息」「相关日程」「注意事项」等）
CREATE TABLE IF NOT EXISTS agent_wiki_sections (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    page_id UUID NOT NULL REFERENCES agent_wiki_pages(id) ON DELETE CASCADE,
    heading VARCHAR(120) NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 来源引用：每条事实都能追溯到它来自哪条五维数据
CREATE TABLE IF NOT EXISTS agent_wiki_sources (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    section_id UUID NOT NULL REFERENCES agent_wiki_sections(id) ON DELETE CASCADE,
    source_kind VARCHAR(32) NOT NULL,
    source_id UUID,
    excerpt TEXT NOT NULL DEFAULT ''
);

-- 变更日志：append-only，由应用侧写入（不是模型）
CREATE TABLE IF NOT EXISTS agent_wiki_changelog (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    page_id UUID REFERENCES agent_wiki_pages(id) ON DELETE SET NULL,
    entity_type VARCHAR(32) NOT NULL,
    entity_key VARCHAR(64) NOT NULL,
    -- created / updated / merged / retracted
    action VARCHAR(32) NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_wiki_pages_family
    ON agent_wiki_pages (family_id, entity_type);
CREATE INDEX IF NOT EXISTS idx_agent_wiki_sections_page
    ON agent_wiki_sections (page_id, sort_order);
CREATE INDEX IF NOT EXISTS idx_agent_wiki_sources_section
    ON agent_wiki_sources (section_id);
CREATE INDEX IF NOT EXISTS idx_agent_wiki_changelog_family
    ON agent_wiki_changelog (family_id, created_at DESC);

-- ===== 技能包 =====

-- 每个家庭安装的技能版本；文件在磁盘，这里只做索引与开关
CREATE TABLE IF NOT EXISTS agent_skills (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    skill_id VARCHAR(64) NOT NULL,
    version VARCHAR(32) NOT NULL,
    name VARCHAR(120) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- 相对 skills 根目录的路径，如 'family-harmony/1.0.0'
    path TEXT NOT NULL,
    -- 该技能允许使用的工具白名单（空 = 不额外授权，服务端仍逐个鉴权）
    tools TEXT[] NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (family_id, skill_id, version)
);

CREATE INDEX IF NOT EXISTS idx_agent_skills_family
    ON agent_skills (family_id, enabled);

-- ===== 写操作审批 =====

CREATE TABLE IF NOT EXISTS agent_write_ops (
    operation_id UUID PRIMARY KEY,
    family_id UUID NOT NULL,
    -- 谁提的（成员）；为空表示家庭公共
    member_id UUID,
    -- create_task / create_event
    kind VARCHAR(32) NOT NULL,
    payload JSONB NOT NULL,
    -- proposed / confirmed / rejected / failed / expired
    status VARCHAR(16) NOT NULL DEFAULT 'proposed'
        CHECK (status IN ('proposed', 'confirmed', 'rejected', 'failed', 'expired')),
    -- 给用户看的预览文案（确认前先看这个）
    preview TEXT NOT NULL DEFAULT '',
    -- 落库结果（确认后的返回体）
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_write_ops_family
    ON agent_write_ops (family_id, status, created_at DESC);
