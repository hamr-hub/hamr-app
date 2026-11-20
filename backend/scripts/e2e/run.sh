#!/usr/bin/env bash
# 家庭版 Agent 端到端验证。
#
# 跑一条完整链路：
#   真实 Postgres → 真实后端进程 → 假模型（OpenAI 兼容）
#   Wiki 重建 → Wiki 检索 → 场景包安装/开关 → 写操作提议 → 人工确认 → 待办落库
#   → 模型体检 → 成员维度会话
#
# 用法： bash scripts/e2e/run.sh
# 依赖： docker、python3、curl、jq、cargo
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKEND="$(cd "$HERE/../.." && pwd)"
PG_CONTAINER="hamr-e2e-$$"
PG_PORT="${PG_PORT:-55439}"
MOCK_PORT="${MOCK_PORT:-18081}"
API_PORT="${API_PORT:-3399}"
DB_URL="postgresql://hamr:hamr@127.0.0.1:${PG_PORT}/hamr"
FAMILY="11111111-2222-3333-4444-555555555555"
API="http://127.0.0.1:${API_PORT}"

FAILED=0
pass() { printf '  \033[32m✓\033[0m %s\n' "$1"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAILED=$((FAILED+1)); }
step() { printf '\n\033[1m%s\033[0m\n' "$1"; }

cleanup() {
  # 显式 wait 掉被杀掉的子进程 —— 否则 bash 退出时会打印 "Terminated: 15"
  # 的作业控制噪声，混淆测试结果。
  if [[ -n "${BACKEND_PID:-}" ]]; then
    kill "$BACKEND_PID" 2>/dev/null
    wait "$BACKEND_PID" 2>/dev/null
  fi
  if [[ -n "${MOCK_PID:-}" ]]; then
    kill "$MOCK_PID" 2>/dev/null
    wait "$MOCK_PID" 2>/dev/null
  fi
  docker rm -f "$PG_CONTAINER" >/dev/null 2>&1
}
trap cleanup EXIT

# 上一轮残留的进程会继续监听端口，导致本轮后端绑定失败，
# 而健康检查却打到旧进程上 —— 表现为"表不存在"的诡异 500。
# 开跑前先把端口清干净。
free_port() {
  local port="$1" pids
  pids=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
  if [[ -n "$pids" ]]; then
    kill $pids 2>/dev/null || true
    sleep 1
    pids=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
    [[ -n "$pids" ]] && kill -9 $pids 2>/dev/null || true
    sleep 1
  fi
}
free_port "$API_PORT"
free_port "$MOCK_PORT"

# ---------- 起依赖 ----------
step "启动依赖"
docker rm -f "$PG_CONTAINER" >/dev/null 2>&1
docker run -d --name "$PG_CONTAINER" \
  -e POSTGRES_USER=hamr -e POSTGRES_PASSWORD=hamr -e POSTGRES_DB=hamr \
  -p "${PG_PORT}:5432" postgres:16-alpine >/dev/null || { fail "docker run 失败"; exit 1; }

for _ in $(seq 1 60); do
  if docker exec "$PG_CONTAINER" pg_isready -U hamr -d hamr >/dev/null 2>&1; then break; fi
  sleep 1
done
docker exec "$PG_CONTAINER" pg_isready -U hamr -d hamr >/dev/null 2>&1 \
  && pass "Postgres 就绪 (${PG_PORT})" || { fail "Postgres 未就绪"; exit 1; }

python3 "$HERE/mock_model.py" --port "$MOCK_PORT" >/tmp/hamr-e2e-mock.log 2>&1 &
MOCK_PID=$!
sleep 1
curl -fsS "http://127.0.0.1:${MOCK_PORT}/" >/dev/null 2>&1 \
  && pass "假模型就绪 (${MOCK_PORT})" || { fail "假模型未就绪"; exit 1; }

step "编译后端"
( cd "$BACKEND" && cargo build ) >/tmp/hamr-e2e-build.log 2>&1 \
  && pass "cargo build 成功" \
  || { fail "cargo build 失败（见 /tmp/hamr-e2e-build.log）"; tail -30 /tmp/hamr-e2e-build.log; exit 1; }

step "启动后端"
(
  cd "$BACKEND"
  export DATABASE_URL="$DB_URL"
  export PORT="$API_PORT"
  export HAMR_AGENT_ENABLED=true
  export HAMR_AGENT_PROVIDER=local
  export HAMR_AGENT_BASE_URL="http://127.0.0.1:${MOCK_PORT}/v1"
  export HAMR_AGENT_MODEL=mock
  export HAMR_AGENT_SKILLS_DIR="$BACKEND/skills"
  export HAMR_AGENT_ALLOW_WRITE=true
  export HAMR_AGENT_MAX_STEPS=6
  export RUST_LOG=warn
  # exec 让子 shell 被服务端进程替换，$! 才是真正的服务端 PID，
  # 否则 cleanup 杀掉的是 shell，服务端会变成残留进程继续占用端口。
  exec ./target/debug/hamr-app-server
) >/tmp/hamr-e2e-server.log 2>&1 &
BACKEND_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS "${API}/api/v1/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
# 必须确认本轮的服务端活着、且没有绑定端口失败 —— 否则健康检查会打到
# 别人的旧进程上，后续断言会以"表不存在"的方式诡异地挂掉。
if ! kill -0 "$BACKEND_PID" 2>/dev/null; then
  fail "后端进程已退出（见 /tmp/hamr-e2e-server.log）"
  tail -40 /tmp/hamr-e2e-server.log
  exit 1
fi
if grep -q "Address already in use" /tmp/hamr-e2e-server.log; then
  fail "后端端口被占用（见 /tmp/hamr-e2e-server.log）"
  exit 1
fi
curl -fsS "${API}/api/v1/health" >/dev/null 2>&1 \
  && pass "后端就绪 (${API_PORT})" \
  || { fail "后端未就绪（见 /tmp/hamr-e2e-server.log）"; tail -40 /tmp/hamr-e2e-server.log; exit 1; }

# ---------- 造数据 ----------
step "造家庭数据"
curl -fsS -X POST "${API}/api/v1/people" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"张伟\",\"role\":\"爸爸\"}" >/dev/null \
  && pass "成员 张伟" || fail "创建成员 张伟 失败"
curl -fsS -X POST "${API}/api/v1/people" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"李娜\",\"role\":\"妈妈\"}" >/dev/null \
  && pass "成员 李娜" || fail "创建成员 李娜 失败"
curl -fsS -X POST "${API}/api/v1/things" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"牛奶\",\"category\":\"食品\"}" >/dev/null \
  && pass "物品 牛奶" || fail "创建物品 牛奶 失败"
curl -fsS -X POST "${API}/api/v1/spaces" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"name\":\"厨房\",\"type\":\"房间\"}" >/dev/null \
  && pass "空间 厨房" || fail "创建空间 厨房 失败"

# ---------- 1. 家庭 Wiki ----------
step "1. 家庭 Wiki（确定性重建 + 两段式检索）"
REBUILD=$(curl -fsS -X POST "${API}/api/v1/agent/wiki/rebuild" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
PAGES=$(echo "$REBUILD" | jq -r '.pages // 0')
[[ "$PAGES" -gt 0 ]] && pass "重建出 ${PAGES} 个实体页" || fail "重建页数为 0：${REBUILD}"

REBUILD2=$(curl -fsS -X POST "${API}/api/v1/agent/wiki/rebuild" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
CHANGED2=$(echo "$REBUILD2" | jq -r '.changed // 0')
[[ "$CHANGED2" == "0" ]] && pass "重复重建幂等（changed=0）" || fail "重建不幂等：changed=${CHANGED2}"

INDEX=$(curl -fsS "${API}/api/v1/agent/wiki?family_id=${FAMILY}")
echo "$INDEX" | jq -e 'map(select(.title=="张伟")) | length > 0' >/dev/null \
  && pass "索引含成员页 张伟" || fail "索引缺少 张伟：${INDEX}"

HITS=$(curl -fsS "${API}/api/v1/agent/wiki/search?family_id=${FAMILY}&query=牛奶")
echo "$HITS" | jq -e 'length > 0' >/dev/null \
  && pass "检索命中 牛奶" || fail "检索未命中：${HITS}"

PAGE=$(curl -fsS "${API}/api/v1/agent/wiki/page?family_id=${FAMILY}&entity_type=member&key=张伟")
echo "$PAGE" | jq -e '.page.title=="张伟"' >/dev/null \
  && pass "取到成员页详情（含来源）" || fail "成员页详情异常：${PAGE}"

# ---------- 2. 场景包 ----------
step "2. 场景包（版本化 + 每家庭开关 + 只收敛不授权）"
AVAIL=$(curl -fsS "${API}/api/v1/agent/skills/available")
echo "$AVAIL" | jq -e 'map(select(.skill_id=="family-harmony")) | length > 0' >/dev/null \
  && pass "磁盘上可见 family-harmony" || fail "未扫描到场景包：${AVAIL}"

curl -fsS -X POST "${API}/api/v1/agent/skills/install" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"skill_id\":\"family-harmony\",\"version\":\"1.0.0\",\"enabled\":true}" >/dev/null \
  && pass "安装并启用 family-harmony@1.0.0" || fail "安装场景包失败"

LIST=$(curl -fsS "${API}/api/v1/agent/skills?family_id=${FAMILY}")
echo "$LIST" | jq -e 'map(select(.skill_id=="family-harmony" and .enabled==true)) | length == 1' >/dev/null \
  && pass "已安装列表中为启用状态" || fail "启用状态异常：${LIST}"

curl -fsS -X POST "${API}/api/v1/agent/skills/enabled" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"skill_id\":\"family-harmony\",\"version\":\"1.0.0\",\"enabled\":false}" >/dev/null
LIST2=$(curl -fsS "${API}/api/v1/agent/skills?family_id=${FAMILY}")
echo "$LIST2" | jq -e 'map(select(.skill_id=="family-harmony"))[0].enabled == false' >/dev/null \
  && pass "停用生效" || fail "停用未生效：${LIST2}"

# ---------- 3. 审批写操作 ----------
step "3. 审批写操作（提议 → 人工确认 → 落库）"
CHAT=$(curl -fsS -X POST "${API}/api/v1/agent/chat" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"message\":\"请提议创建待办：交物业费\",\"member_id\":null}")
echo "$CHAT" | jq -e '.steps | map(select(.tool=="propose_task")) | length > 0' >/dev/null \
  && pass "模型发起了 propose_task 提议" || fail "未发起提议：${CHAT}"

OPS=$(curl -fsS "${API}/api/v1/agent/write-ops?family_id=${FAMILY}&status=proposed")
OPID=$(echo "$OPS" | jq -r '.[0].operation_id // empty')
[[ -n "$OPID" ]] && pass "待确认提议 ${OPID}" || fail "没有待确认提议：${OPS}"

TASKS_BEFORE=$(curl -fsS "${API}/api/v1/tasks?family_id=${FAMILY}" | jq 'length')
[[ "$TASKS_BEFORE" == "0" ]] && pass "确认前业务表无写入" || fail "确认前已有 ${TASKS_BEFORE} 条待办"

CONFIRM=$(curl -fsS -X POST "${API}/api/v1/agent/write-ops/${OPID}/confirm" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}")
echo "$CONFIRM" | jq -e '.status=="confirmed"' >/dev/null \
  && pass "确认成功" || fail "确认失败：${CONFIRM}"

TASKS_AFTER=$(curl -fsS "${API}/api/v1/tasks?family_id=${FAMILY}")
echo "$TASKS_AFTER" | jq -e 'length == 1' >/dev/null \
  && pass "待办已落库" || fail "落库条数异常：${TASKS_AFTER}"
echo "$TASKS_AFTER" | jq -e 'map(select(.title=="E2E 交物业费")) | length == 1' >/dev/null \
  && pass "待办标题正确" || fail "待办标题异常：${TASKS_AFTER}"

curl -fsS -X POST "${API}/api/v1/agent/write-ops/${OPID}/confirm" \
  -H 'Content-Type: application/json' -d "{\"family_id\":\"${FAMILY}\"}" >/dev/null
TASKS_IDEM=$(curl -fsS "${API}/api/v1/tasks?family_id=${FAMILY}" | jq 'length')
[[ "$TASKS_IDEM" == "1" ]] && pass "重复确认幂等（仍 1 条）" || fail "重复确认产生 ${TASKS_IDEM} 条待办"

# ---------- 4. 模型体检 ----------
step "4. 模型体检"
PROBE=$(curl -fsS -X POST "${API}/api/v1/agent/probe")
echo "$PROBE" | jq -e '.passed == true' >/dev/null \
  && pass "体检通过" || fail "体检未通过：${PROBE}"
echo "$PROBE" | jq -e '.score == 3 and .verdict == "usable"' >/dev/null \
  && pass "评分 3 / usable" || fail "评分异常：${PROBE}"

# ---------- 5. 成员维度会话 ----------
step "5. 成员维度会话"
PEOPLE=$(curl -fsS "${API}/api/v1/people?family_id=${FAMILY}")
MEMBER_ID=$(echo "$PEOPLE" | jq -r '.[] | select(.name=="李娜") | .id')
CHAT2=$(curl -fsS -X POST "${API}/api/v1/agent/chat" -H 'Content-Type: application/json' \
  -d "{\"family_id\":\"${FAMILY}\",\"message\":\"你好\",\"member_id\":\"${MEMBER_ID}\"}")
SID=$(echo "$CHAT2" | jq -r '.session_id // empty')
SESS=$(curl -fsS "${API}/api/v1/agent/sessions?family_id=${FAMILY}")
echo "$SESS" | jq -e --arg m "$MEMBER_ID" 'map(select(.id==$m or .member_id==$m)) | length > 0' >/dev/null \
  && pass "会话已绑定成员 ${MEMBER_ID}" || fail "会话未绑定成员：${SESS}"

echo
if [[ "$FAILED" == "0" ]]; then
  printf '\033[32m端到端验证全部通过\033[0m\n'
  exit 0
else
  printf '\033[31m端到端验证失败 %d 项\033[0m\n' "$FAILED"
  exit 1
fi
