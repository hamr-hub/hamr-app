# HamR 管家应用 (app.hamr.store)

HamR 平台的核心数据管理应用，实现家庭"人时事物境"五维管理。

## 功能

| 维度 | 功能 |
|------|------|
| 人 | 家庭成员档案（姓名/角色/生日/联系方式） |
| 时 | 家庭日历（日程/提醒/分类） |
| 事 | 事务管理（待办/里程碑/优先级/进度） |
| 物 | 物品资产（登记/分类/位置/过期提醒） |
| 境 | 生活空间（客厅/卧室等空间定义） |

## 技术栈

| 端 | 技术 |
|----|------|
| 后端 | Rust + Axum + SQLx + PostgreSQL |
| 前端 | React 19 + Tailwind CSS v4 + Zustand + date-fns |
| 认证 | JWT（与 hamr-account 共享 secret） |

## 快速启动

```bash
cp .env.example .env
docker compose up -d
```

访问 http://localhost:3010（需先在 hamr-account 登录获取 token）

## API

| 资源 | 路径 |
|------|------|
| 统计 | GET `/api/v1/dashboard?family_id=` |
| 人员 | CRUD `/api/v1/people` |
| 日历 | CRUD `/api/v1/events` |
| 事务 | CRUD `/api/v1/tasks` |
| 物品 | CRUD `/api/v1/things` |
| 空间 | CRUD `/api/v1/spaces` |

## 管家 Agent（`/api/v1/agent/*`）

本地优先、模型可插拔的家庭 Agent 运行时。**默认关闭** —— 未设置 `HAMR_AGENT_ENABLED=true`
时相关端点统一返回 503。

| 能力 | 路径 |
|------|------|
| 对话 | POST `/api/v1/agent/chat` |
| 会话 / 消息 | GET `/api/v1/agent/sessions`、`/sessions/:id/messages`、DELETE `/sessions/:id` |
| 长期记忆 | GET/POST `/api/v1/agent/memories` |
| 主动提醒 | GET `/api/v1/agent/reminders`、POST `/api/v1/agent/proactive/{scan,flush}` |
| 家庭 Wiki | GET `/api/v1/agent/wiki`、`/wiki/search`、`/wiki/page`、`/wiki/changelog`、POST `/api/v1/agent/wiki/rebuild` |
| 场景包 | GET `/api/v1/agent/skills`、`/skills/available`、POST `/api/v1/agent/skills/{install,enabled}` |
| 写操作审批 | GET `/api/v1/agent/write-ops`、POST `/api/v1/agent/write-ops/:id/{confirm,reject}` |
| 模型体检 | POST `/api/v1/agent/probe` |

### 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `HAMR_AGENT_ENABLED` | `false` | 总开关 |
| `HAMR_AGENT_PROVIDER` | `local` | `local`（Ollama 等 OpenAI 兼容本地端点）/ `cloud` |
| `HAMR_AGENT_BASE_URL` | `http://127.0.0.1:11434/v1` | 模型端点 |
| `HAMR_AGENT_MODEL` | `qwen2.5:7b` | 模型名 |
| `HAMR_AGENT_API_KEY` | — | 云端必填 |
| `HAMR_AGENT_ALLOW_PII` | `false` | 云端路径下是否允许原始 PII 出网 |
| `HAMR_AGENT_MAX_STEPS` | `8` | 单轮「思考→调工具」循环上限 |
| `HAMR_AGENT_QUIET_HOURS` | `22:00-07:30` | 免打扰时段（主动提醒） |
| `HAMR_AGENT_DAILY_REMINDER_CAP` | `5` | 每日主动提醒条数上限 |
| `HAMR_AGENT_SKILLS_DIR` | `skills` | 场景包根目录 |
| `HAMR_AGENT_ALLOW_WRITE` | `false` | 是否允许 Agent 发起写操作**提议**（落库仍需人工确认） |

### 不变量

Agent 默认关闭 · 工具只读（v1）· `family_id` 由服务端注入 · 出网必脱敏 · 步数有界 ·
写操作必须经人显式确认且工具表内无 confirm 类工具 · 场景包只收敛工具面不授权 ·
Wiki 页面只有重建器能写。

### 开发

以下命令从 `hamr-app` 仓库根目录运行：

```bash
# 单元测试
cargo test --manifest-path backend/Cargo.toml --locked --offline --all-targets

# 端到端验证（真实 PostgreSQL + 真实后端进程 + 模拟模型，CI 运行）
bash backend/scripts/e2e/run.sh
```

端到端脚本使用临时数据库、设备身份和共享密钥，不读取 `~/.hamr`。
它设置 `HAMR_APP_LOCAL_ONLY=true`，让后端只监听 `127.0.0.1` 且不启动 P2P。
测试端口被占用时直接退出，不终止其他进程；可用 `PG_PORT`、`API_PORT` 和
`MOCK_PORT` 覆盖端口。脚本只调用本次运行创建的模拟端点，不需要安装、启动或下载真实模型。

场景包格式见 [`backend/skills/README.md`](backend/skills/README.md)。
