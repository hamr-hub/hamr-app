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
