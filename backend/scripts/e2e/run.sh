#!/usr/bin/env bash
# 家庭版 Agent 端到端验证。
#
# 跑一条完整链路：
#   真实 Postgres → 真实后端进程 → 假模型（OpenAI 兼容）
#   Wiki 重建 → Wiki 检索 → 场景包安装/开关 → 写操作提议 → 人工确认 → 待办落库
#   → 模型体检 → 成员维度会话
#
# 用法：bash backend/scripts/e2e/run.sh
# 仅运行模拟模型，不下载或调用真实模型。
# 依赖：docker、python3、curl、jq、cargo
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKEND="$(cd "$HERE/../.." && pwd)"
PG_PORT="${PG_PORT:-55439}"
MOCK_PORT="${MOCK_PORT:-18081}"
API_PORT="${API_PORT:-3399}"
DB_URL="postgresql://hamr:hamr@127.0.0.1:${PG_PORT}/hamr"
FAMILY="11111111-2222-3333-4444-555555555555"
API="http://127.0.0.1:${API_PORT}"
RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hamr-e2e.XXXXXXXX") || exit 1

FAILED=0
pass() { printf '  \033[32m✓\033[0m %s\n' "$1"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAILED=$((FAILED+1)); }
step() { printf '\n\033[1m%s\033[0m\n' "$1"; }
api_curl() { curl --noproxy '*' -fsS --max-time 180 -H "Authorization: Bearer $API_TOKEN" "$@"; }

cleanup() {
  if [[ -n "${BACKEND_PID:-}" ]]; then
    kill "$BACKEND_PID" 2>/dev/null || true
    wait "$BACKEND_PID" 2>/dev/null || true
  fi
  if [[ -n "${MOCK_PID:-}" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
  if [[ -n "${PG_CONTAINER:-}" ]]; then
    docker rm -fv "$PG_CONTAINER" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$RUN_DIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
API_TOKEN=$(python3 -I -c 'import secrets; print(secrets.token_hex(24))') || exit 1

# 预检使用绑定而非终止任何现有监听者。若预检后又发生抢占，启动必须失败。
python3 -I - "$API_PORT" "$PG_PORT" "$MOCK_PORT" <<'PY' || exit 1
import socket
import sys
from contextlib import ExitStack

with ExitStack() as stack:
    for value in sys.argv[1:]:
        try:
            port = int(value)
            if not 1 <= port <= 65535:
                raise ValueError("port must be between 1 and 65535")
            listener = stack.enter_context(socket.socket())
            # 与 Rust / Python 服务端一致：允许复用处于 TIME_WAIT 的本地端口，
            # 否则上一轮刚结束时会把可用端口误判成被占用。
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("0.0.0.0", port))
        except (ValueError, OSError) as error:
            sys.exit(f"测试端口 {value} 不可用：{error}")
PY

step "启动依赖"
python3 -I "$HERE/mock_model.py" --port "$MOCK_PORT" >"$RUN_DIR/mock.log" 2>&1 &
MOCK_PID=$!
mock_ready() { grep -q 'mock model listening' "$RUN_DIR/mock.log" && kill -0 "$MOCK_PID" 2>/dev/null; }
for _ in $(seq 1 20); do
  if mock_ready || ! kill -0 "$MOCK_PID" 2>/dev/null; then break; fi
  sleep 1
done
if ! mock_ready; then
  fail "模拟模型未就绪"
  tail -30 "$RUN_DIR/mock.log"
  exit 1
fi
pass "模拟模型就绪 (${MOCK_PORT})"

PG_CONTAINER=$(docker run -d \
  -e POSTGRES_USER=hamr -e POSTGRES_PASSWORD=hamr -e POSTGRES_DB=hamr \
  -p "127.0.0.1:${PG_PORT}:5432" postgres:16-alpine) || { fail "docker run 失败"; exit 1; }

pg_ready() { docker exec "$PG_CONTAINER" pg_isready -h 127.0.0.1 -U hamr -d hamr >/dev/null 2>&1; }
for _ in $(seq 1 60); do
  if pg_ready; then break; fi
  sleep 1
done
pg_ready && pass "Postgres 就绪 (${PG_PORT})" || { fail "Postgres 未就绪"; docker logs "$PG_CONTAINER"; exit 1; }

step "编译后端"
( cd "$BACKEND" && CARGO_TARGET_DIR="$BACKEND/target" cargo build --locked --offline ) >"$RUN_DIR/build.log" 2>&1 \
  && pass "cargo build 成功" \
  || { fail "cargo build 失败"; tail -30 "$RUN_DIR/build.log"; exit 1; }

step "审批 PostgreSQL 回归"
( cd "$BACKEND" && DATABASE_URL="$DB_URL" CARGO_TARGET_DIR="$BACKEND/target" \
  cargo test --locked --offline agent::writeops::tests::postgres_ -- --ignored ) \
  || { fail "审批事务回归失败"; exit 1; }

step "启动后端"
(
  cd "$RUN_DIR" || exit 1
  export DATABASE_URL="$DB_URL"
  export PORT="$API_PORT"
  export HAMR_APP_LOCAL_ONLY=true
  export HAMR_DATA_DIR="$RUN_DIR/data"
  export HAMR_APP_AUTH_MODE=shared-secret
  export HAMR_APP_SHARED_SECRET="$API_TOKEN"
  export HAMR_AGENT_ENABLED=true
  export HAMR_AGENT_PROVIDER=local
  export HAMR_AGENT_BASE_URL="http://127.0.0.1:${MOCK_PORT}/v1"
  export HAMR_AGENT_MODEL=mock
  export HAMR_AGENT_API_KEY=""
  export HAMR_AGENT_SKILLS_DIR="$BACKEND/skills"
  export HAMR_AGENT_ALLOW_WRITE=true
  export HAMR_AGENT_MAX_STEPS=6
  export HAMR_AGENT_TIMEOUT_SECS=90
  export HAMR_JIABU_URL=""
  export RUST_LOG=warn
  exec "$BACKEND/target/debug/hamr-app-server"
) >"$RUN_DIR/server.log" 2>&1 &
BACKEND_PID=$!

# 受每次运行独立密钥保护的端点用于就绪检查，避免连到其他进程。
backend_ready() {
  api_curl --max-time 2 "${API}/api/v1/agent/status" 2>/dev/null | \
    jq -e '.enabled and .model == "mock"' >/dev/null
}
for _ in $(seq 1 60); do
  if backend_ready || ! kill -0 "$BACKEND_PID" 2>/dev/null; then break; fi
  sleep 1
done
if ! kill -0 "$BACKEND_PID" 2>/dev/null || ! backend_ready; then
  fail "后端未就绪"
  tail -40 "$RUN_DIR/server.log"
  exit 1
fi
pass "后端就绪 (${API_PORT})"

check_probe() {
  local probe
  probe=$(api_curl -X POST "${API}/api/v1/agent/probe")
  jq -e '.passed and .score == 3 and .verdict == "usable"' <<<"$probe" >/dev/null \
    && pass "模型体检 3/3（usable）" || fail "模型体检未通过：${probe}"
  jq -c '{score, verdict, latency_ms}' <<<"$probe"
}

# ---------- 造数据 ----------
step "造家庭数据"
api_curl -X POST "${API}/api/v1/people" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"张伟\",\"role\":\"爸爸\"}" >/dev/null \
  && pass "成员 张伟" || fail "创建成员 张伟 失败"
api_curl -X POST "${API}/api/v1/people" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"李娜\",\"role\":\"妈妈\"}" >/dev/null \
  && pass "成员 李娜" || fail "创建成员 李娜 失败"
api_curl -X POST "${API}/api/v1/things" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"牛奶\",\"category\":\"食品\"}" >/dev/null \
  && pass "物品 牛奶" || fail "创建物品 牛奶 失败"
api_curl -X POST "${API}/api/v1/spaces" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"厨房\",\"type\":\"房间\"}" >/dev/null \
  && pass "空间 厨房" || fail "创建空间 厨房 失败"

# ---------- 1. 家庭 Wiki ----------
step "1. 家庭 Wiki（确定性重建 + 两段式检索）"
REBUILD=$(api_curl -X POST "${API}/api/v1/agent/wiki/rebuild" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
PAGES=$(echo "$REBUILD" | jq -r '.pages // 0')
[[ "$PAGES" -gt 0 ]] && pass "重建出 ${PAGES} 个实体页" || fail "重建页数为 0：${REBUILD}"

REBUILD2=$(api_curl -X POST "${API}/api/v1/agent/wiki/rebuild" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
CHANGED2=$(echo "$REBUILD2" | jq -r '.changed // 0')
[[ "$CHANGED2" == "0" ]] && pass "重复重建幂等（changed=0）" || fail "重建不幂等：changed=${CHANGED2}"

INDEX=$(api_curl "${API}/api/v1/agent/wiki?family_id=${FAMILY}")
echo "$INDEX" | jq -e 'map(select(.title=="张伟")) | length > 0' >/dev/null \
  && pass "索引含成员页 张伟" || fail "索引缺少 张伟：${INDEX}"

HITS=$(api_curl --get "${API}/api/v1/agent/wiki/search" \
  --data-urlencode "family_id=${FAMILY}" --data-urlencode 'query=牛奶')
echo "$HITS" | jq -e 'length > 0' >/dev/null \
  && pass "检索命中 牛奶" || fail "检索未命中：${HITS}"

PAGE=$(api_curl --get "${API}/api/v1/agent/wiki/page" --data-urlencode "family_id=${FAMILY}" \
  --data-urlencode 'entity_type=member' --data-urlencode 'key=张伟')
echo "$PAGE" | jq -e '.page.title=="张伟"' >/dev/null \
  && pass "取到成员页详情（含来源）" || fail "成员页详情异常：${PAGE}"

# ---------- 2. 场景包 ----------
step "2. 场景包（版本化 + 每家庭开关 + 只收敛不授权）"
AVAIL=$(api_curl "${API}/api/v1/agent/skills/available")
echo "$AVAIL" | jq -e 'map(select(.skill_id=="family-harmony")) | length > 0' >/dev/null \
  && pass "磁盘上可见 family-harmony" || fail "未扫描到场景包：${AVAIL}"

api_curl -X POST "${API}/api/v1/agent/skills/install" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"skill_id\":\"family-harmony\",\"version\":\"1.0.0\",\"enabled\":true}" >/dev/null \
  && pass "安装并启用 family-harmony@1.0.0" || fail "安装场景包失败"

LIST=$(api_curl "${API}/api/v1/agent/skills?family_id=${FAMILY}")
echo "$LIST" | jq -e 'map(select(.skill_id=="family-harmony" and .enabled==true)) | length == 1' >/dev/null \
  && pass "已安装列表中为启用状态" || fail "启用状态异常：${LIST}"

api_curl -X POST "${API}/api/v1/agent/skills/enabled" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"skill_id\":\"family-harmony\",\"version\":\"1.0.0\",\"enabled\":false}" >/dev/null
LIST2=$(api_curl "${API}/api/v1/agent/skills?family_id=${FAMILY}")
echo "$LIST2" | jq -e 'map(select(.skill_id=="family-harmony"))[0].enabled == false' >/dev/null \
  && pass "停用生效" || fail "停用未生效：${LIST2}"

# ---------- 3. 审批写操作 ----------
step "3. 审批写操作（提议 → 人工确认 → 落库）"
CHAT=$(api_curl -X POST "${API}/api/v1/agent/chat" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"message\":\"请提议创建待办：交物业费\",\"member_id\":null}")
echo "$CHAT" | jq -e '.steps | map(select(.tool=="propose_task")) | length > 0' >/dev/null \
  && pass "模型发起了 propose_task 提议" || fail "未发起提议：${CHAT}"

OPS=$(api_curl "${API}/api/v1/agent/write-ops?family_id=${FAMILY}&status=proposed")
OPID=$(echo "$OPS" | jq -r '.[0].operation_id // empty')
[[ -n "$OPID" ]] && pass "待确认提议 ${OPID}" || fail "没有待确认提议：${OPS}"

TASKS_BEFORE=$(api_curl "${API}/api/v1/tasks?family_id=${FAMILY}" | jq 'length')
[[ "$TASKS_BEFORE" == "0" ]] && pass "确认前业务表无写入" || fail "确认前已有 ${TASKS_BEFORE} 条待办"

CONFIRM=$(api_curl -X POST "${API}/api/v1/agent/write-ops/${OPID}/confirm" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
echo "$CONFIRM" | jq -e '.status=="confirmed"' >/dev/null \
  && pass "确认成功" || fail "确认失败：${CONFIRM}"

TASKS_AFTER=$(api_curl "${API}/api/v1/tasks?family_id=${FAMILY}")
echo "$TASKS_AFTER" | jq -e 'length == 1' >/dev/null \
  && pass "待办已落库" || fail "落库条数异常：${TASKS_AFTER}"
echo "$TASKS_AFTER" | jq -e 'map(select(.title=="E2E 交物业费")) | length == 1' >/dev/null \
  && pass "待办标题正确" || fail "待办标题异常：${TASKS_AFTER}"

api_curl -X POST "${API}/api/v1/agent/write-ops/${OPID}/confirm" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}" >/dev/null
TASKS_IDEM=$(api_curl "${API}/api/v1/tasks?family_id=${FAMILY}" | jq 'length')
[[ "$TASKS_IDEM" == "1" ]] && pass "重复确认幂等（仍 1 条）" || fail "重复确认产生 ${TASKS_IDEM} 条待办"

# ---------- 4. 工具会话续聊 ----------
step "4. 续聊（完整回放前一轮工具消息）"
CHAT_SID=$(jq -r '.session_id // empty' <<<"$CHAT")
if [[ -n "$CHAT_SID" ]]; then
  FOLLOWUP=$(jq -nc --arg family "$FAMILY" --arg sid "$CHAT_SID" \
    '{family_id:$family,session_id:$sid,message:"续聊验证：有哪些安排？"}')
  FOLLOWUP_RESULT=$(api_curl -X POST "${API}/api/v1/agent/chat" \
    -H 'Content-Type: application/json' -d "$FOLLOWUP")
  jq -e --arg sid "$CHAT_SID" \
    '.session_id == $sid and any(.steps[]; .tool == "list_events")' <<<"$FOLLOWUP_RESULT" >/dev/null \
    && pass "同一会话继续调用 list_events，模拟模型接受完整工具轨迹" \
    || fail "工具会话续聊失败：${FOLLOWUP_RESULT}"
else
  fail "无法续聊：首轮响应没有会话 ID"
fi

# ---------- 5. 模型体检 ----------
step "5. 模型体检"
check_probe

# ---------- 6. 成员维度会话 ----------
step "6. 成员维度会话"
PEOPLE=$(api_curl "${API}/api/v1/people?family_id=${FAMILY}")
MEMBER_ID=$(echo "$PEOPLE" | jq -r '.[] | select(.name=="李娜") | .id')
CHAT2=$(api_curl -X POST "${API}/api/v1/agent/chat" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"message\":\"你好\",\"member_id\":\"${MEMBER_ID}\"}")
SID=$(jq -r '.session_id // empty' <<<"$CHAT2")
SESS=$(api_curl "${API}/api/v1/agent/sessions?family_id=${FAMILY}")
[[ -n "$MEMBER_ID" && -n "$SID" ]] && \
  jq -e --arg member "$MEMBER_ID" --arg sid "$SID" \
    'any(.[]; .id == $sid and .member_id == $member)' <<<"$SESS" >/dev/null \
  && pass "新会话已绑定成员 ${MEMBER_ID}" || fail "会话未绑定成员：${SESS}"

echo
if [[ "$FAILED" == "0" ]]; then
  printf '\033[32m端到端验证全部通过\033[0m\n'
  exit 0
else
  printf '\033[31m端到端验证失败 %d 项\033[0m\n' "$FAILED"
  exit 1
fi
