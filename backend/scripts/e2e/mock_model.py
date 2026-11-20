#!/usr/bin/env python3
"""端到端测试用的假 OpenAI 兼容模型服务。

用途：HamR 后端的 Agent 运行时只依赖 OpenAI Chat Completions 协议，
所以给一个**故意可控**的假模型，就能在没有 Ollama 的机器上把
「对话 → 调工具 → 写提议 → 人工确认 → 落库」整条链路跑完。

行为由最后一条 user 消息里的关键词决定（确定性，无随机）：

| 关键词        | 行为                                        |
|---------------|---------------------------------------------|
| `提议`        | 调 `propose_task`，创建待办提议             |
| `场景`        | 调 `use_skill`，加载场景包                  |
| `知识库`      | 调 `search_wiki`，检索家庭 Wiki             |
| `安排`        | 调 `list_events`，查日程                    |
| 其他 / 工具回灌 | 直接给一句结论，并把工具结果原样带回        |

用法：
    python3 mock_model.py --port 18081
"""
from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = 18081


def _tool_call(call_id: str, name: str, args: dict) -> dict:
    return {
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(args, ensure_ascii=False)},
    }


def _last_user_text(messages: list) -> str:
    for m in reversed(messages):
        if m.get("role") == "user":
            c = m.get("content")
            if isinstance(c, str):
                return c
    return ""


def _tool_results(messages: list) -> list:
    return [m.get("content") or "" for m in messages if m.get("role") == "tool"]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):  # noqa: A002 - 静默，避免污染测试输出
        pass

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            req = json.loads(raw.decode("utf-8"))
        except Exception:
            req = {}

        messages = req.get("messages") or []
        tools = {t.get("function", {}).get("name") for t in (req.get("tools") or [])}
        user_text = _last_user_text(messages)
        results = _tool_results(messages)

        tool_calls = []
        content = ""

        if results:
            # 工具已执行过 —— 把结果原样带回，证明「用了工具结果」
            joined = " | ".join(r for r in results if r)
            content = f"工具返回：{joined[:300]}"
        elif "提议" in user_text and "propose_task" in tools:
            tool_calls.append(
                _tool_call(
                    "call_propose_1",
                    "propose_task",
                    {
                        "title": "E2E 交物业费",
                        "description": "端到端测试产生的待办提议",
                        "priority": "high",
                    },
                )
            )
        elif "场景" in user_text and "use_skill" in tools:
            tool_calls.append(_tool_call("call_skill_1", "use_skill", {"skill_id": "family-harmony"}))
        elif "知识库" in user_text and "search_wiki" in tools:
            tool_calls.append(_tool_call("call_wiki_1", "search_wiki", {"query": "张伟"}))
        elif "安排" in user_text and "list_events" in tools:
            tool_calls.append(_tool_call("call_events_1", "list_events", {"days": 1}))
        else:
            content = "这是模拟模型的回答。"

        payload = {
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 0,
            "model": req.get("model", "mock"),
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls or None,
                    },
                    "finish_reason": "tool_calls" if tool_calls else "stop",
                }
            ],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        body = json.dumps({"status": "ok", "model": "mock"}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=PORT)
    args = ap.parse_args()
    srv = HTTPServer(("127.0.0.1", args.port), Handler)
    print(f"mock model listening on http://127.0.0.1:{args.port}/v1", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
