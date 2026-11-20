//! Agent 运行时 —— HamR 管家从「数据底座」升级为「主动管家」的大脑层。
//!
//! 设计前提（与 P2P 本地化定位一致）：
//!
//! 1. **本地优先**：Agent 运行时跑在用户自己的设备上，默认对接本机模型
//!    （Ollama / 任意 OpenAI 兼容端点），家庭数据不出设备。
//! 2. **模型可插拔**：`LlmProvider` 是 trait，云端模型是「显式开启」的另一种实现，
//!    不是默认路径。
//! 3. **出网必须脱敏**：接云端模型时，默认对成员姓名 / 电话 / 邮箱 / 证件号做
//!    一致性替换（成员A、成员B……），需要用户显式放开才传原始值。
//! 4. **失败关闭**：Agent 未显式启用时端点返回 503，不会「静默降级成假管家」。
//! 5. **只读优先**：v1 的工具集只读五维数据，唯一可写的是 Agent 自己的长期记忆
//!    表，不会替用户改写家庭数据。
//!
//! 模块分工：
//!
//! - [`config`]  —— 环境变量配置与校验（含 API Key 的日志脱敏）
//! - [`provider`] —— LLM 抽象与 OpenAI 兼容实现（本地 / 云端共用一套协议）
//! - [`privacy`]  —— PII 脱敏器
//! - [`prompt`]   —— 管家人格与系统提示词
//! - [`tools`]    —— 工具定义与执行分发
//! - [`store`]    —— 会话 / 消息 / 长期记忆 / 提醒的持久化抽象
//! - [`runner`]   —— 主循环（思考 → 调工具 → 观察 → 结论）
//! - [`proactive`] —— 主动提醒：三态决策 + 闸门 + 规则扫描
//! - [`wiki`]      —— 家庭 Wiki：实体页 + 来源引用 + 变更日志（模型只读）
//! - [`skills`]    —— 技能包：版本化 SKILL.md + 服务端工具白名单
//! - [`writeops`]  —— 审批写操作：模型提议 → 用户确认 → 幂等落库
//! - [`probe`]     —— 模型体检：判断模型会不会调工具、会不会用工具结果

pub mod config;
pub mod privacy;
pub mod probe;
pub mod prompt;
pub mod proactive;
pub mod provider;
pub mod runner;
pub mod skills;
pub mod store;
pub mod tools;
pub mod wiki;
pub mod writeops;

pub use config::AgentConfig;
pub use runner::{AgentError, AgentRuntime, AgentStatus, StepTrace};
pub use store::{AgentMemory, AgentSession, ConversationStore, MemoryKind, PgStore};
