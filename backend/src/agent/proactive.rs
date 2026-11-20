//! 主动提醒内核 —— 管家从「问才答」走向「该说才说」。
//!
//! 设计参照 2026-11-20《Linger 基座调研与架构借鉴》中的主动 Agent 模式，
//! 由 HamR 在 Rust 内重新实现，不依赖 LLM：
//!
//! - **三态决策**：每个候选信号独立判一次，结果为告知 / 稍后跟进 / 保持沉默。
//! - **闸门**：免打扰时段（支持跨午夜）、每日条数上限、紧急旁路、
//!   去重（同一事项同一状态每天只判一次）。
//! - **规则驱动扫描**：信号直接来自五维数据表（临期物品、临近日程、逾期待办），
//!   不经过模型 —— 不出网、不涉脱敏，也不会被提示词注入操纵。
//!   LLM 润色是未来可选项，且必须走云端脱敏路径。
//! - **手机屏消息**：第一行结论，中间事实，结尾唯一动作。
//!
//! 持久化两条线：`agent_reminders` 存事项本身（同家庭同去重键唯一），
//! `agent_proactive_log` 存每次判定的流水，闸门靠流水去重与限流。

use std::collections::HashSet;

use chrono::{DateTime, Duration, Local, NaiveTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use super::config::AgentConfig;
use super::store::{ConversationStore, StoreError};

/// 三态决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// 立即告知。
    Tell,
    /// 暂不打扰，挂起等免打扰结束 / 次日再发。
    FollowUp,
    /// 保持沉默（已告知过 / 已跟进过，不重复判定）。
    Silent,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Tell => "tell",
            Decision::FollowUp => "follow_up",
            Decision::Silent => "silent",
        }
    }
}

/// 紧急度。紧急信号可绕过免打扰与条数上限（但绕不过去重）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Urgent,
}

impl Urgency {
    pub fn as_str(self) -> &'static str {
        match self {
            Urgency::Normal => "normal",
            Urgency::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "urgent" => Urgency::Urgent,
            _ => Urgency::Normal,
        }
    }
}

/// 提醒生命周期状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderStatus {
    Pending,
    Told,
    Snoozed,
    Dismissed,
}

impl ReminderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReminderStatus::Pending => "pending",
            ReminderStatus::Told => "told",
            ReminderStatus::Snoozed => "snoozed",
            ReminderStatus::Dismissed => "dismissed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pending" => Some(ReminderStatus::Pending),
            "told" => Some(ReminderStatus::Told),
            "snoozed" => Some(ReminderStatus::Snoozed),
            "dismissed" => Some(ReminderStatus::Dismissed),
            _ => None,
        }
    }
}

/// 一条提醒（`agent_reminders` 行）。
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct AgentReminder {
    pub id: Uuid,
    pub family_id: Uuid,
    pub kind: String,
    pub subject: Uuid,
    pub state: String,
    pub title: String,
    pub body: String,
    pub action: String,
    pub urgency: String,
    pub status: String,
    pub dedupe_key: String,
    pub fire_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentReminder {
    fn to_upsert(&self, status: ReminderStatus, fire_at: DateTime<Utc>) -> ReminderUpsert {
        ReminderUpsert {
            kind: self.kind.clone(),
            subject: self.subject,
            state: self.state.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            action: self.action.clone(),
            urgency: Urgency::parse(&self.urgency),
            status,
            fire_at,
        }
    }
}

/// 主动判定流水行（`agent_proactive_log`）。
#[derive(Clone, Debug, FromRow)]
pub struct ProactiveLogRow {
    pub dedupe_key: String,
    pub decision: String,
    pub created_at: DateTime<Utc>,
}

/// 扫描出的原始候选信号（尚未经过闸门）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderCandidate {
    pub kind: &'static str,
    pub subject: Uuid,
    /// 事项状态指纹：状态变了（改期、续期）指纹就变，需要重新判定一次。
    pub state: String,
    pub title: String,
    pub body: String,
    pub action: String,
    pub urgency: Urgency,
}

impl ReminderCandidate {
    pub fn dedupe_key(&self) -> String {
        format!("{}:{}:{}", self.kind, self.subject, self.state)
    }
}

/// upsert 进 `agent_reminders` 的内容。
#[derive(Clone, Debug)]
pub struct ReminderUpsert {
    pub kind: String,
    pub subject: Uuid,
    pub state: String,
    pub title: String,
    pub body: String,
    pub action: String,
    pub urgency: Urgency,
    pub status: ReminderStatus,
    pub fire_at: DateTime<Utc>,
}

/// 一次判定的完整记录（提醒 upsert + 流水）。
#[derive(Clone, Debug)]
pub struct ProactiveRecord {
    pub family_id: Uuid,
    pub dedupe_key: String,
    pub decision: Decision,
    pub reason: String,
    /// 判定发生的时刻 —— 流水按这个时间记账，而不是按写入时刻。
    /// 这样补跑/回放历史扫描时，「同一天只判一次」的边界仍然正确。
    pub at: DateTime<Utc>,
    pub reminder: Option<ReminderUpsert>,
}

/// 闸门判定所处的阶段。
///
/// 同一个去重键在两个阶段含义不同：
/// - `New`：这是一条**新信号**（扫描时发现）。今天已跟进过的信号不应重复登记，
///   否则家庭会收到一堆重复的挂起提醒。
/// - `Release`：这是一条**待放行的挂起提醒**。「已跟进过」正是当初挂起它的
///   原因，若据此静默，提醒将永远发不出去 —— 所以这一阶段忽略该规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatePhase {
    New,
    Release,
}

/// 闸门判定结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateVerdict {
    pub decision: Decision,
    pub reason: &'static str,
}

/// 当天已有的判定（从流水聚合）。
#[derive(Debug, Default)]
pub struct TodayLog {
    told: HashSet<String>,
    followed: HashSet<String>,
    told_count: u32,
}

impl TodayLog {
    fn from_rows(rows: &[ProactiveLogRow]) -> Self {
        let mut log = TodayLog::default();
        for r in rows {
            match r.decision.as_str() {
                "tell" => {
                    log.told.insert(r.dedupe_key.clone());
                    log.told_count += 1;
                }
                "follow_up" => {
                    log.followed.insert(r.dedupe_key.clone());
                }
                _ => {}
            }
        }
        log
    }
}

// ===== 闸门（纯函数） =====

/// 当前分钟数是否落在免打扰区间。区间支持跨午夜（如 22:00-07:30）。
/// 起止相等视为「不设免打扰」。
pub fn in_quiet(minute_of_day: u16, quiet_start: u16, quiet_end: u16) -> bool {
    if quiet_start == quiet_end {
        return false;
    }
    if quiet_start < quiet_end {
        minute_of_day >= quiet_start && minute_of_day < quiet_end
    } else {
        minute_of_day >= quiet_start || minute_of_day < quiet_end
    }
}

/// 闸门核心：对一个去重键给出三态判定。
///
/// 判定顺序是刻意的：
/// 1. 今天已告知过 → 沉默（去重最高优先，紧急事项也不该一天发两遍）；
/// 2. 紧急 → 告知（绕过免打扰与条数上限）；
/// 3. 免打扰时段 → 跟进；
/// 4. 达到每日上限 → 跟进；
/// 5. 今天已判过「跟进」→ 沉默（挂起的提醒会在放行时统一发，不必重复登记）；
/// 6. 其余 → 告知。
pub fn gate_decide(
    cfg: &AgentConfig,
    minute_of_day: u16,
    urgency: Urgency,
    dedupe_key: &str,
    today: &TodayLog,
    phase: GatePhase,
) -> GateVerdict {
    if today.told.contains(dedupe_key) {
        return GateVerdict {
            decision: Decision::Silent,
            reason: "dedupe_told_today",
        };
    }
    if urgency == Urgency::Urgent {
        return GateVerdict {
            decision: Decision::Tell,
            reason: "urgent_bypass",
        };
    }
    if in_quiet(minute_of_day, cfg.quiet_start, cfg.quiet_end) {
        return GateVerdict {
            decision: Decision::FollowUp,
            reason: "quiet_hours",
        };
    }
    if today.told_count >= cfg.daily_reminder_cap {
        return GateVerdict {
            decision: Decision::FollowUp,
            reason: "daily_cap",
        };
    }
    if phase == GatePhase::New && today.followed.contains(dedupe_key) {
        return GateVerdict {
            decision: Decision::Silent,
            reason: "dedupe_followed_today",
        };
    }
    GateVerdict {
        decision: Decision::Tell,
        reason: "eligible",
    }
}

/// 下一次「免打扰结束」时刻（本地时区换算 UTC），用于挂起提醒的 fire_at。
fn next_fire(now: DateTime<Utc>, quiet_end: u16) -> DateTime<Utc> {
    let local = now.with_timezone(&Local);
    let end_hour = quiet_end / 60;
    let end_minute = quiet_end % 60;
    let naive = local
        .date_naive()
        .and_hms_opt(end_hour as u32, end_minute as u32, 0)
        .expect("valid quiet-end time");
    // DST 缝隙 / 重叠时 single() 可能为 None，取 earliest 兜底。
    let today_end = match Local.from_local_datetime(&naive).single() {
        Some(t) => t,
        None => Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap_or(local),
    };
    let today_end = today_end.with_timezone(&Utc);
    if today_end > now {
        today_end
    } else {
        today_end + Duration::days(1)
    }
}

/// 本地时区当天零点的 UTC 时刻 —— 「每日」计数按家庭本地日界。
fn local_midnight_utc(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&Local);
    let midnight = local.date_naive().and_time(NaiveTime::MIN);
    Local
        .from_local_datetime(&midnight)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(now)
}

// ===== 手机屏消息格式 =====

/// 拼一条手机屏提醒：第一行结论，中间事实，结尾唯一动作。
pub fn format_notification(title: &str, body: &str, action: &str) -> String {
    let mut s = title.trim().to_string();
    let body = body.trim();
    if !body.is_empty() {
        s.push('\n');
        s.push_str(body);
    }
    if !action.trim().is_empty() {
        s.push_str("\n👉 ");
        s.push_str(action.trim());
    }
    s
}

// ===== 扫描报告（HTTP 返回） =====

/// 本轮确定告知的一条提醒。
#[derive(Clone, Debug, Serialize)]
pub struct ToldItem {
    pub reminder_id: Uuid,
    pub kind: String,
    pub urgency: String,
    pub title: String,
    pub body: String,
    pub action: String,
    pub dedupe_key: String,
    /// 已按手机屏格式拼好的消息文本。
    pub message: String,
}

/// 单个候选的判定明细（便于前端展示与排查「为什么没提醒我」）。
#[derive(Clone, Debug, Serialize)]
pub struct VerdictItem {
    pub dedupe_key: String,
    pub decision: Decision,
    pub reason: String,
}

/// 一次扫描的结果。
#[derive(Clone, Debug, Serialize)]
pub struct ScanReport {
    /// 扫描到的候选信号总数。
    pub scanned: usize,
    /// 本轮告知。
    pub told: Vec<ToldItem>,
    /// 挂起（免打扰 / 条数上限）。
    pub follow_up: usize,
    /// 沉默（去重）。
    pub silent: usize,
    /// 逐条判定明细。
    pub decisions: Vec<VerdictItem>,
}

// ===== 评估与扫描 =====

/// 对候选信号逐个过闸门并落库。纯逻辑 + 存储，不查五维表 —— 便于单测。
pub async fn evaluate(
    store: &dyn ConversationStore,
    family_id: Uuid,
    now: DateTime<Utc>,
    cfg: &AgentConfig,
    candidates: Vec<ReminderCandidate>,
) -> Result<ScanReport, StoreError> {
    let minute_of_day = {
        let local = now.with_timezone(&Local);
        (local.hour() * 60 + local.minute()) as u16
    };
    let since = local_midnight_utc(now);
    let rows = store.proactive_since(family_id, since).await?;
    let mut today = TodayLog::from_rows(&rows);

    let mut report = ScanReport {
        scanned: candidates.len(),
        told: Vec::new(),
        follow_up: 0,
        silent: 0,
        decisions: Vec::new(),
    };

    // 同一轮扫描里也可能产出重复键（理论上 SQL 已不重，双保险）。
    let mut seen_this_run: HashSet<String> = HashSet::new();

    for c in candidates {
        let key = c.dedupe_key();

        if !seen_this_run.insert(key.clone()) {
            store
                .record_proactive(ProactiveRecord {
                    family_id,
                    dedupe_key: key.clone(),
                    decision: Decision::Silent,
                    reason: "dedupe_within_scan".into(),
                    at: now,
                    reminder: None,
                })
                .await?;
            report.silent += 1;
            report.decisions.push(VerdictItem {
                dedupe_key: key,
                decision: Decision::Silent,
                reason: "dedupe_within_scan".into(),
            });
            continue;
        }

        let verdict = gate_decide(cfg, minute_of_day, c.urgency, &key, &today, GatePhase::New);

        match verdict.decision {
            Decision::Tell => {
                let reminder = ReminderUpsert {
                    kind: c.kind.to_string(),
                    subject: c.subject,
                    state: c.state.clone(),
                    title: c.title.clone(),
                    body: c.body.clone(),
                    action: c.action.clone(),
                    urgency: c.urgency,
                    status: ReminderStatus::Told,
                    fire_at: now,
                };
                let id = store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: key.clone(),
                        decision: Decision::Tell,
                        reason: verdict.reason.into(),
                        at: now,
                        reminder: Some(reminder),
                    })
                    .await?
                    .unwrap_or_default();
                today.told.insert(key.clone());
                today.told_count += 1;
                report.told.push(ToldItem {
                    reminder_id: id,
                    kind: c.kind.to_string(),
                    urgency: c.urgency.as_str().to_string(),
                    message: format_notification(&c.title, &c.body, &c.action),
                    title: c.title,
                    body: c.body,
                    action: c.action,
                    dedupe_key: key.clone(),
                });
            }
            Decision::FollowUp => {
                let fire_at = next_fire(now, cfg.quiet_end);
                let reminder = ReminderUpsert {
                    kind: c.kind.to_string(),
                    subject: c.subject,
                    state: c.state.clone(),
                    title: c.title.clone(),
                    body: c.body.clone(),
                    action: c.action.clone(),
                    urgency: c.urgency,
                    status: ReminderStatus::Pending,
                    fire_at,
                };
                store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: key.clone(),
                        decision: Decision::FollowUp,
                        reason: verdict.reason.into(),
                        at: now,
                        reminder: Some(reminder),
                    })
                    .await?;
                today.followed.insert(key.clone());
                report.follow_up += 1;
            }
            Decision::Silent => {
                store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: key.clone(),
                        decision: Decision::Silent,
                        reason: verdict.reason.into(),
                        at: now,
                        reminder: None,
                    })
                    .await?;
                report.silent += 1;
            }
        }

        report.decisions.push(VerdictItem {
            dedupe_key: key,
            decision: verdict.decision,
            reason: verdict.reason.to_string(),
        });
    }

    Ok(report)
}

/// 从五维数据表扫描候选信号。
///
/// - 临期/已过期物品：`things.expiry_date` 在未来 7 天内或已过期（已过期判紧急）；
/// - 临近日程：未来 24 小时内开始，2 小时内开始或已到点判紧急；
/// - 临期/逾期待办：24 小时内到期或已逾期，紧急优先级或已逾期判紧急。
///
/// 「久未联系」需要联系记录表，当前五维模型没有该数据，暂不实现，
/// 待 PROJ-019 后续在 people 上补 last_contacted_at 后接入（同一套闸门）。
pub async fn fetch_candidates(
    db: &sqlx::PgPool,
    family_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<ReminderCandidate>, String> {
    let mut out = Vec::new();

    // --- 临期物品 ---
    let things = sqlx::query_as::<_, crate::models::Thing>(
        "SELECT * FROM things
         WHERE family_id = $1 AND status = 'active'
           AND expiry_date IS NOT NULL
           AND expiry_date <= CURRENT_DATE + INTERVAL '7 days'
         ORDER BY expiry_date ASC
         LIMIT 100",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("scan things failed: {e}"))?;

    let today = now.with_timezone(&Local).date_naive();
    for t in things {
        let Some(exp) = t.expiry_date else { continue };
        let days = (exp - today).num_days();
        let (title, body, urgency) = if days < 0 {
            (
                format!("「{}」已过期", t.name),
                format!("已过期 {} 天，建议尽快处理", -days),
                Urgency::Urgent,
            )
        } else if days == 0 {
            (
                format!("「{}」今天到期", t.name),
                "今天就是保质期 / 有效期最后一天".into(),
                Urgency::Urgent,
            )
        } else {
            (
                format!("「{}」快到期了", t.name),
                format!("还有 {days} 天到期（{}）", exp),
                Urgency::Normal,
            )
        };
        let location = t
            .location
            .as_deref()
            .map(|l| format!("（{l}）"))
            .unwrap_or_default();
        out.push(ReminderCandidate {
            kind: "expiring_thing",
            subject: t.id,
            state: exp.to_string(),
            title,
            body: format!("{body}{location}"),
            action: format!("检查或处理「{}」", t.name),
            urgency,
        });
    }

    // --- 临近日程（未来 24h） ---
    let events = sqlx::query_as::<_, crate::models::Event>(
        "SELECT * FROM events
         WHERE family_id = $1 AND start_time >= $2 AND start_time <= $3
         ORDER BY start_time ASC
         LIMIT 100",
    )
    .bind(family_id)
    .bind(now)
    .bind(now + Duration::hours(24))
    .fetch_all(db)
    .await
    .map_err(|e| format!("scan events failed: {e}"))?;

    for e in events {
        let local_start = e.start_time.with_timezone(&Local);
        let hours = (e.start_time - now).num_hours();
        let urgency = if hours <= 2 {
            Urgency::Urgent
        } else {
            Urgency::Normal
        };
        let when = if e.all_day {
            format!("{}（全天）", local_start.format("%m-%d"))
        } else {
            local_start.format("%m-%d %H:%M").to_string()
        };
        let loc = e
            .location
            .as_deref()
            .filter(|l| !l.is_empty())
            .map(|l| format!("，地点：{l}"))
            .unwrap_or_default();
        out.push(ReminderCandidate {
            kind: "upcoming_event",
            subject: e.id,
            // 指纹精确到小时：改期超过一小时就会被当作新状态重新提醒。
            state: e.start_time.format("%Y%m%d%H").to_string(),
            title: format!("临近日程：{}", e.title),
            body: format!("开始时间：{when}{loc}"),
            action: format!("查看「{}」的安排", e.title),
            urgency,
        });
    }

    // --- 逾期 / 临期待办（24h 内到期） ---
    let tasks = sqlx::query_as::<_, crate::models::Task>(
        "SELECT * FROM tasks
         WHERE family_id = $1 AND status IN ('todo','in_progress')
           AND due_date IS NOT NULL AND due_date <= $2
         ORDER BY due_date ASC
         LIMIT 100",
    )
    .bind(family_id)
    .bind(now + Duration::hours(24))
    .fetch_all(db)
    .await
    .map_err(|e| format!("scan tasks failed: {e}"))?;

    for t in tasks {
        let Some(due) = t.due_date else { continue };
        let overdue = due < now;
        let urgency = if overdue || t.priority == "urgent" {
            Urgency::Urgent
        } else {
            Urgency::Normal
        };
        let due_local = due.with_timezone(&Local).format("%m-%d %H:%M");
        let (title, body) = if overdue {
            let hours = (now - due).num_hours().max(1);
            (
                format!("待办已逾期：{}", t.title),
                format!("原定 {due_local}，已逾期约 {hours} 小时"),
            )
        } else {
            (
                format!("待办快到期：{}", t.title),
                format!("截止时间：{due_local}"),
            )
        };
        out.push(ReminderCandidate {
            kind: "overdue_task",
            subject: t.id,
            state: due.format("%Y%m%d%H").to_string(),
            title,
            body,
            action: format!("完成或重新安排「{}」", t.title),
            urgency,
        });
    }

    Ok(out)
}

/// 完整扫描：查五维表 → 过闸门 → 落库。
pub async fn scan(
    db: &sqlx::PgPool,
    store: &dyn ConversationStore,
    family_id: Uuid,
    now: DateTime<Utc>,
    cfg: &AgentConfig,
) -> Result<ScanReport, StoreError> {
    let candidates = fetch_candidates(db, family_id, now)
        .await
        .map_err(StoreError::Db)?;
    evaluate(store, family_id, now, cfg, candidates).await
}

/// 放行已挂起的提醒（免打扰结束 / 新的一天后由调度方调用）。
///
/// 仍要重过一遍闸门：紧急的立即告知；仍受限的继续挂起并推迟 fire_at；
/// 今天已经告知过的沉默掉。
pub async fn flush_due(
    store: &dyn ConversationStore,
    family_id: Uuid,
    now: DateTime<Utc>,
    cfg: &AgentConfig,
) -> Result<Vec<ToldItem>, StoreError> {
    let due: Vec<AgentReminder> = store
        .reminders(family_id, Some(ReminderStatus::Pending), 100)
        .await?
        .into_iter()
        .filter(|r| r.fire_at <= now)
        .collect();
    if due.is_empty() {
        return Ok(Vec::new());
    }

    let minute_of_day = {
        let local = now.with_timezone(&Local);
        (local.hour() * 60 + local.minute()) as u16
    };
    let rows = store
        .proactive_since(family_id, local_midnight_utc(now))
        .await?;
    let today = TodayLog::from_rows(&rows);

    let mut told = Vec::new();
    for r in due {
        let urgency = Urgency::parse(&r.urgency);
        let verdict = gate_decide(
            cfg,
            minute_of_day,
            urgency,
            &r.dedupe_key,
            &today,
            GatePhase::Release,
        );
        match verdict.decision {
            Decision::Tell => {
                store
                    .update_reminder_status(family_id, r.id, ReminderStatus::Told)
                    .await?;
                store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: r.dedupe_key.clone(),
                        decision: Decision::Tell,
                        reason: "due_flush".into(),
                        at: now,
                        reminder: None,
                    })
                    .await?;
                told.push(ToldItem {
                    reminder_id: r.id,
                    kind: r.kind.clone(),
                    urgency: r.urgency.clone(),
                    message: format_notification(&r.title, &r.body, &r.action),
                    title: r.title,
                    body: r.body,
                    action: r.action,
                    dedupe_key: r.dedupe_key,
                });
            }
            Decision::FollowUp => {
                let fire_at = next_fire(now, cfg.quiet_end);
                store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: r.dedupe_key.clone(),
                        decision: Decision::FollowUp,
                        reason: format!("reflush_{}", verdict.reason),
                        at: now,
                        reminder: Some(r.to_upsert(ReminderStatus::Pending, fire_at)),
                    })
                    .await?;
            }
            Decision::Silent => {
                store
                    .record_proactive(ProactiveRecord {
                        family_id,
                        dedupe_key: r.dedupe_key.clone(),
                        decision: Decision::Silent,
                        reason: verdict.reason.into(),
                        at: now,
                        reminder: None,
                    })
                    .await?;
            }
        }
    }
    Ok(told)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::ProviderKind;
    use crate::agent::store::MemStore;
    use std::sync::Arc;

    fn cfg() -> AgentConfig {
        AgentConfig {
            enabled: true,
            provider: ProviderKind::Local,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "test".into(),
            api_key: None,
            allow_pii: false,
            max_steps: 4,
            timeout: std::time::Duration::from_secs(5),
            max_history: 20,
            jiabu_url: None,
            quiet_start: 22 * 60,
            quiet_end: 7 * 60 + 30,
            daily_reminder_cap: 5,
        }
    }

    fn candidate(urgency: Urgency, key_state: &str) -> ReminderCandidate {
        ReminderCandidate {
            kind: "expiring_thing",
            subject: Uuid::new_v4(),
            state: key_state.into(),
            title: "牛奶快到期了".into(),
            body: "还有 2 天到期".into(),
            action: "检查冰箱".into(),
            urgency,
        }
    }

    #[test]
    fn quiet_hours_cross_midnight() {
        // 22:00-07:30
        assert!(in_quiet(23 * 60, 22 * 60, 7 * 60 + 30));
        assert!(in_quiet(0, 22 * 60, 7 * 60 + 30));
        assert!(in_quiet(7 * 60 + 29, 22 * 60, 7 * 60 + 30));
        assert!(!in_quiet(7 * 60 + 30, 22 * 60, 7 * 60 + 30));
        assert!(!in_quiet(12 * 60, 22 * 60, 7 * 60 + 30));
        assert!(!in_quiet(21 * 60 + 59, 22 * 60, 7 * 60 + 30));
    }

    #[test]
    fn quiet_disabled_when_start_equals_end() {
        assert!(!in_quiet(0, 0, 0));
        assert!(!in_quiet(12 * 60, 9 * 60, 9 * 60));
    }

    #[test]
    fn gate_tells_in_daytime() {
        let v = gate_decide(
            &cfg(),
            12 * 60,
            Urgency::Normal,
            "k",
            &TodayLog::default(),
            GatePhase::New,
        );
        assert_eq!(
            v,
            GateVerdict {
                decision: Decision::Tell,
                reason: "eligible"
            }
        );
    }

    #[test]
    fn gate_defers_in_quiet_hours() {
        let v = gate_decide(
            &cfg(),
            23 * 60,
            Urgency::Normal,
            "k",
            &TodayLog::default(),
            GatePhase::New,
        );
        assert_eq!(v.decision, Decision::FollowUp);
        assert_eq!(v.reason, "quiet_hours");
    }

    #[test]
    fn gate_urgent_bypasses_quiet_and_cap() {
        let today = TodayLog {
            told_count: 100,
            ..Default::default()
        };
        let v = gate_decide(&cfg(), 2 * 60, Urgency::Urgent, "k", &today, GatePhase::New);
        assert_eq!(v.decision, Decision::Tell);
        assert_eq!(v.reason, "urgent_bypass");
    }

    #[test]
    fn gate_dedupe_told_wins_over_urgency() {
        let mut today = TodayLog::default();
        today.told.insert("k".into());
        let v = gate_decide(
            &cfg(),
            12 * 60,
            Urgency::Urgent,
            "k",
            &today,
            GatePhase::New,
        );
        assert_eq!(v.decision, Decision::Silent);
        assert_eq!(v.reason, "dedupe_told_today");
    }

    #[test]
    fn gate_daily_cap_defers() {
        let today = TodayLog {
            told_count: 5, // 上限 5
            ..Default::default()
        };
        let v = gate_decide(
            &cfg(),
            12 * 60,
            Urgency::Normal,
            "k",
            &today,
            GatePhase::New,
        );
        assert_eq!(v.decision, Decision::FollowUp);
        assert_eq!(v.reason, "daily_cap");
    }

    #[test]
    fn gate_followed_today_goes_silent() {
        let mut today = TodayLog::default();
        today.followed.insert("k".into());
        let v = gate_decide(
            &cfg(),
            12 * 60,
            Urgency::Normal,
            "k",
            &today,
            GatePhase::New,
        );
        assert_eq!(v.decision, Decision::Silent);
        assert_eq!(v.reason, "dedupe_followed_today");
    }

    #[test]
    fn release_phase_ignores_followed_dedup() {
        // 「已跟进过」正是当初挂起它的原因 —— 放行阶段若据此静默，
        // 提醒就永远发不出去。
        let mut today = TodayLog::default();
        today.followed.insert("k".into());
        let v = gate_decide(
            &cfg(),
            12 * 60,
            Urgency::Normal,
            "k",
            &today,
            GatePhase::Release,
        );
        assert_eq!(v.decision, Decision::Tell, "挂起的提醒必须能放行");
    }

    #[test]
    fn notification_is_phone_screen_shaped() {
        let msg = format_notification("牛奶快到期了", "还有 2 天到期", "检查冰箱");
        let lines: Vec<&str> = msg.lines().collect();
        assert_eq!(lines[0], "牛奶快到期了", "第一行必须是结论");
        assert_eq!(lines.last(), Some(&"👉 检查冰箱"), "结尾必须是唯一动作");
    }

    #[test]
    fn next_fire_is_after_now() {
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let fire = next_fire(now, 7 * 60 + 30);
        assert!(fire > now);
        // 本地时区下 fire 的分钟数应等于 quiet_end
        let local = fire.with_timezone(&Local);
        assert_eq!(local.hour() * 60 + local.minute(), 7 * 60 + 30);
    }

    #[tokio::test]
    async fn evaluate_tells_then_dedupes_on_second_scan() {
        let store: Arc<MemStore> = Arc::new(MemStore::new());
        let fam = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();

        let c = candidate(Urgency::Normal, "2026-11-25");
        let key = c.dedupe_key();
        let report1 = evaluate(store.as_ref(), fam, now, &cfg(), vec![c.clone()])
            .await
            .unwrap();
        assert_eq!(report1.told.len(), 1);
        assert_eq!(report1.follow_up, 0);
        assert_eq!(report1.silent, 0);

        // 同一天再扫：同事项同状态必须沉默
        let report2 = evaluate(store.as_ref(), fam, now, &cfg(), vec![c])
            .await
            .unwrap();
        assert_eq!(report2.told.len(), 0);
        assert_eq!(report2.silent, 1);
        assert_eq!(report2.decisions[0].reason, "dedupe_told_today");

        // 提醒落库为 told
        let reminders = store.reminders(fam, None, 10).await.unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].status, "told");
        assert_eq!(reminders[0].dedupe_key, key);
    }

    #[tokio::test]
    async fn evaluate_defers_at_night_and_flush_releases() {
        let store: Arc<MemStore> = Arc::new(MemStore::new());
        let fam = Uuid::new_v4();
        let night = Utc.with_ymd_and_hms(2026, 11, 20, 23, 0, 0).unwrap();

        let c = candidate(Urgency::Normal, "2026-11-25");
        let report = evaluate(store.as_ref(), fam, night, &cfg(), vec![c])
            .await
            .unwrap();
        assert_eq!(report.follow_up, 1);
        assert_eq!(report.told.len(), 0);

        let pending = store
            .reminders(fam, Some(ReminderStatus::Pending), 10)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].fire_at > night, "fire_at 必须排在夜间之后");

        // 次日早上放行
        let morning = Utc.with_ymd_and_hms(2026, 11, 21, 8, 0, 0).unwrap();
        let told = flush_due(store.as_ref(), fam, morning, &cfg())
            .await
            .unwrap();
        assert_eq!(told.len(), 1, "免打扰结束后应放行挂起提醒");

        // 再 flush 一次不会重复告知
        let again = flush_due(store.as_ref(), fam, morning, &cfg())
            .await
            .unwrap();
        assert!(again.is_empty());
    }

    #[tokio::test]
    async fn evaluate_caps_daily_tells() {
        let store: Arc<MemStore> = Arc::new(MemStore::new());
        let fam = Uuid::new_v4();
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let cs: Vec<_> = (0..8)
            .map(|i| candidate(Urgency::Normal, &format!("d{i}")))
            .collect();
        let report = evaluate(store.as_ref(), fam, now, &cfg(), cs)
            .await
            .unwrap();
        assert_eq!(report.told.len(), 5, "每日上限 5 条");
        assert_eq!(report.follow_up, 3, "超出的挂起");
    }
}
