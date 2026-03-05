# HamR 管家应用 (app.hamr.store)

> HamR 核心数据管理应用 - 五维家庭数据管理平台

[![Status](https://img.shields.io/badge/status-开发中-yellow)](https://github.com/hamr-hub/hamr-app)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Backend](https://img.shields.io/badge/backend-Rust+Axum-orange)](https://github.com/tokio-rs/axum)
[![Frontend](https://img.shields.io/badge/frontend-React+TypeScript-61dafb)](https://react.dev)

## 📋 项目概述

**项目编号**: PROJ-005  
**域名**: app.hamr.store  
**优先级**: ⭐⭐⭐ 高  
**状态**: 待开发  

HamR 管家是平台的核心应用，提供五维家庭数据管理（人/时/事/物/境），为 JiaBu 决策系统提供结构化数据基础。

## 🎯 五维管理体系

### 1️⃣ 人 - 成员管理
- **成员档案**: 基本信息、角色、生日、联系方式
- **关系图谱**: 家庭成员关系可视化
- **重要日期**: 生日提醒、纪念日管理

### 2️⃣ 时 - 时间管理
- **家庭日历**: 共享日程、事件同步
- **日程管理**: 个人/家庭日程
- **时间统计**: 时间分配分析、时间投入可视化

### 3️⃣ 事 - 事务管理
- **待办清单**: 个人/家庭任务管理
- **大事记**: 家庭重要事件记录
- **决策记录**: 重大决策过程与结果

### 4️⃣ 物 - 物品管理
- **家庭资产**: 房产、车辆、贵重物品
- **物品分类**: 家具、电器、图书等
- **消耗品管理**: 库存跟踪、采购提醒

### 5️⃣ 境 - 环境管理
- **环境监测**: 温度、湿度、空气质量
- **场景管理**: 智能家居场景设置
- **空间管理**: 房间布局、物品位置

## 🏗️ 系统架构

```
┌─────────────────┐
│   Frontend      │  React SPA
│  (app.hamr.*)   │  数据可视化
└────────┬────────┘
         │ HTTPS + WebSocket
┌────────▼────────┐
│   Backend       │  Rust + Axum
│   API Server    │  业务逻辑
└────────┬────────┘
         │
    ┌────┴─────┬────────────┬──────────┐
    │          │            │          │
┌───▼───┐  ┌──▼───┐  ┌────▼────┐  ┌──▼───┐
│Postgre│  │Redis │  │ MongoDB │  │  S3  │
│  SQL  │  │Cache │  │ 时序数据 │  │ 文件 │
└────────┘  └──────┘  └─────────┘  └──────┘
```

## 🛠️ 技术栈

### 后端 (backend/)
| 技术 | 用途 | 备注 |
|-----|------|------|
| **Rust** | 编程语言 | 高性能 |
| **Axum** | Web 框架 | 异步框架 |
| **SQLx** | PostgreSQL ORM | 关系数据 |
| **MongoDB** | 时序数据库 | 环境监测数据 |
| **Redis** | 缓存 | 会话/实时数据 |
| **S3 (MinIO)** | 对象存储 | 文件/图片 |

### 前端 (frontend/)
| 技术 | 用途 | 备注 |
|-----|------|------|
| **React 18** | UI 框架 | TypeScript |
| **Vite** | 构建工具 | 快速开发 |
| **TanStack Query** | 数据管理 | 缓存/同步 |
| **Recharts** | 数据可视化 | 图表库 |
| **Tailwind CSS** | 样式框架 | 响应式 |
| **React Flow** | 关系图谱 | 可视化 |

## 🚀 快速开始

### 前置要求
- Rust 1.75+
- Node.js 20+
- PostgreSQL 15+
- MongoDB 6+
- Redis 7+

### 后端启动

```bash
cd backend

# 配置环境变量
cp .env.example .env

# 数据库迁移
sqlx migrate run

# 开发模式
cargo run

# 生产构建
cargo build --release
```

### 前端启动

```bash
cd frontend

# 安装依赖
npm install

# 开发模式
npm run dev

# 生产构建
npm run build
```

## 📦 项目结构

```
hamr-app/
├── backend/
│   ├── src/
│   │   ├── modules/
│   │   │   ├── person/        # 人员管理
│   │   │   ├── time/          # 时间管理
│   │   │   ├── task/          # 事务管理
│   │   │   ├── item/          # 物品管理
│   │   │   └── environment/   # 环境管理
│   │   ├── models/
│   │   ├── services/
│   │   └── utils/
│   ├── migrations/
│   └── Cargo.toml
├── frontend/
│   ├── src/
│   │   ├── pages/
│   │   │   ├── Dashboard.tsx  # 数据看板
│   │   │   ├── Person/        # 人员管理页面
│   │   │   ├── Time/          # 时间管理页面
│   │   │   ├── Task/          # 事务管理页面
│   │   │   ├── Item/          # 物品管理页面
│   │   │   └── Environment/   # 环境管理页面
│   │   ├── components/
│   │   ├── hooks/
│   │   └── api/
│   └── package.json
└── README.md
```

## 📊 数据模型

### 核心实体

**人员 (Person)**
```sql
CREATE TABLE persons (
  id UUID PRIMARY KEY,
  family_id UUID REFERENCES families(id),
  name VARCHAR(100),
  role VARCHAR(50),
  birthday DATE,
  avatar_url TEXT,
  created_at TIMESTAMP
);
```

**任务 (Task)**
```sql
CREATE TABLE tasks (
  id UUID PRIMARY KEY,
  family_id UUID,
  title VARCHAR(200),
  priority VARCHAR(20),
  status VARCHAR(20),
  due_date TIMESTAMP,
  assignee_id UUID REFERENCES persons(id)
);
```

**物品 (Item)**
```sql
CREATE TABLE items (
  id UUID PRIMARY KEY,
  family_id UUID,
  name VARCHAR(200),
  category VARCHAR(50),
  location VARCHAR(100),
  quantity INT,
  purchase_date DATE
);
```

## 🔌 API 端点

### 人员管理
```
GET    /api/persons              # 获取成员列表
POST   /api/persons              # 添加成员
GET    /api/persons/:id          # 获取成员详情
PATCH  /api/persons/:id          # 更新成员信息
DELETE /api/persons/:id          # 删除成员
GET    /api/persons/relationships # 获取关系图谱
```

### 时间管理
```
GET    /api/calendar/events      # 获取日历事件
POST   /api/calendar/events      # 创建事件
PATCH  /api/calendar/events/:id  # 更新事件
DELETE /api/calendar/events/:id  # 删除事件
GET    /api/time/stats           # 时间统计
```

### 事务管理
```
GET    /api/tasks                # 获取任务列表
POST   /api/tasks                # 创建任务
PATCH  /api/tasks/:id            # 更新任务
DELETE /api/tasks/:id            # 删除任务
GET    /api/milestones           # 大事记
```

### 物品管理
```
GET    /api/items                # 获取物品列表
POST   /api/items                # 添加物品
PATCH  /api/items/:id            # 更新物品
DELETE /api/items/:id            # 删除物品
GET    /api/items/consumables    # 消耗品管理
```

### 环境管理
```
GET    /api/environment/sensors  # 传感器数据
POST   /api/environment/readings # 上报数据
GET    /api/environment/scenes   # 场景管理
GET    /api/environment/spaces   # 空间管理
```

## 📊 数据看板

### 概览指标
- 家庭成员总数
- 待办任务数量
- 今日日程数量
- 消耗品提醒数量

### 可视化图表
- **时间分配饼图**: 工作/家庭/个人时间占比
- **任务完成率**: 本周/本月任务完成趋势
- **物品分类分布**: 各类物品数量统计
- **环境监测曲线**: 温湿度历史数据

## 🔐 数据主权

### 导入/导出
- 支持 JSON/CSV 格式
- 批量导入历史数据
- 全量导出备份

### 备份/恢复
- 自动定时备份
- 手动备份
- 一键恢复

### 端到端加密
- 敏感数据加密存储
- 传输层 TLS 加密
- 密钥用户自持

## 📊 里程碑

- [ ] **2026-03-20**: 需求确认
- [ ] **2026-04-05**: 数据模型设计
- [ ] **2026-04-30**: 后端开发
- [ ] **2026-05-20**: 前端开发
- [ ] **2026-05-30**: 测试上线

## 🔗 相关服务

- [账号中心](https://account.hamr.store) - 身份认证
- [JiaBu 决策](https://jiabu.hamr.store) - 智能决策
- [API 服务](https://api.hamr.top) - 数据接口

## 🤝 贡献指南

1. Fork 本仓库
2. 创建功能分支 (`git checkout -b feature/NewFeature`)
3. 提交更改 (`git commit -m 'feat: Add NewFeature'`)
4. 推送到分支 (`git push origin feature/NewFeature`)
5. 开启 Pull Request

## 📄 许可证

MIT License - 详见 [LICENSE](LICENSE)

## 👥 维护者

**HamR Team** - [GitHub Organization](https://github.com/hamr-hub)

---

**最后更新**: 2026-03-05  
**项目状态**: 待开发  
**部署环境**: https://app.hamr.store (即将上线)
