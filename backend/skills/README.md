# 场景包（Skills）

场景包是 HamR 家庭 Agent 的「场景剧本」：一段 Markdown，告诉模型**在某个家庭场景下该怎么问、怎么答、哪些事不能做**。对应 Linger 的 Skill，但做了一处关键收窄 —— **技能只引导，不授权**。

## 目录结构

```
skills/
└── <skill_id>/          # 场景标识（kebab-case），如 family-harmony
    └── <version>/       # 语义化版本，如 1.0.0
        ├── SKILL.md     # 必需
        └── references/  # 可选：补充资料（纯文本，不执行）
```

发布后**不可原地修改** —— 改了就是发了新版本，否则已启用家庭的行为无法复现。

## SKILL.md 格式

```markdown
---
name: 家庭关系沟通
description: 家庭成员之间出现沟通摩擦、误会或冷战时被问到才加载。
version: 1.0.0
tools: [list_people, search_wiki, wiki_page]
---

# 家庭关系沟通

（正文：给模型看的操作指引）
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` | ✅ | 展示名（中文即可） |
| `description` | 建议 | **给模型看的** —— 模型据此判断要不要加载。必须写清「什么时候用」而不是「这是什么」 |
| `version` | 建议 | 语义化版本；不写按 `0.0.0` |
| `tools` | 可选 | 该场景**期望使用**的工具白名单。空 = 不额外收敛 |

## `tools` 白名单的三条规矩

1. **只收敛，不扩张** —— 白名单只能*排除*工具，不能*新增*权限。服务端对每一次工具调用独立鉴权。
2. **只写已知工具名** —— 出现未知工具名时技能包被拒绝安装。
3. **知识层工具始终保留** —— `use_skill` / `search_wiki` / `wiki_page` 不被白名单排除，否则模型加载场景包后就再也换不了场景、查不了详情。

## 禁止事项

- ❌ **不允许带 `scripts/` 目录** —— 与「不执行模型可控代码」的不变量冲突，带脚本的技能包被拒绝安装。
- ❌ **不允许覆盖安全不变量** —— 服务端拼装提示词时安全段始终置后，技能文本替换不掉它。
- ❌ **不允许在技能里塞家庭数据** —— 技能包是随发行版分发的静态内容，不是数据入口。

## 安装与开关

技能文件在磁盘，数据库（`agent_skills`）只记索引与开关，**每个家庭可分别启用 / 停用**。

```bash
# 磁盘上有哪些可安装的技能
GET  /api/v1/agent/skills/available
# 本家庭已安装的技能
GET  /api/v1/agent/skills?family_id=<uuid>
# 安装并启用
POST /api/v1/agent/skills/install
     {"family_id":"<uuid>","skill_id":"family-harmony","version":"1.0.0","enabled":true}
# 停用
POST /api/v1/agent/skills/enabled
     {"family_id":"<uuid>","skill_id":"family-harmony","version":"1.0.0","enabled":false}
```

启用后，场景包目录（名称 + 适用场景）会进系统提示词；模型判断需要时用 `use_skill` 加载正文。

---

**维护者**：HamR Team
**最后更新**：2026-11-20
