-- Agent 运行时：会话、消息、长期记忆
--
-- 与 P2P 五维数据表的关系：这些表只服务 Agent 运行时本身，
-- 不参与 P2P 同步（sync_log 的表白名单里也没有它们）——
-- 会话是设备本地的对话上下文，不该在家庭设备之间漫游。

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- 会话
CREATE TABLE IF NOT EXISTS agent_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    title TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 消息（含工具调用与工具结果）
CREATE TABLE IF NOT EXISTS agent_messages (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id UUID NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    role VARCHAR(20) NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    content TEXT NOT NULL DEFAULT '',
    -- 模型发起的工具调用（OpenAI 格式的 tool_calls 数组）
    tool_calls JSONB,
    -- role = 'tool' 时，指向被回应的 assistant 工具调用 id
    tool_call_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 长期记忆：用户的偏好、约定、长期事实
CREATE TABLE IF NOT EXISTS agent_memories (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    family_id UUID NOT NULL,
    content TEXT NOT NULL,
    source VARCHAR(32) NOT NULL DEFAULT 'agent',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_family_id ON agent_sessions(family_id);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_updated_at ON agent_sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_messages_session_id ON agent_messages(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_memories_family_id ON agent_memories(family_id, created_at DESC);
