"""Toy MCP server for spike S2: one tool, `approve`, used as --permission-prompt-tool.

Speaks newline-delimited JSON-RPC on stdio. Logs every call to the file given as argv[1]
and allows the request unchanged. Throwaway code.
"""
import json
import sys

LOG = sys.argv[1] if len(sys.argv) > 1 else None


def log(obj):
    if LOG:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj) + "\n")


def reply(msg_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
    sys.stdout.flush()


TOOL = {
    "name": "approve",
    "description": "Permission prompt handler for AMIRI spike S2.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "tool_name": {"type": "string"},
            "input": {"type": "object"},
            "tool_use_id": {"type": "string"},
        },
        "required": ["tool_name", "input"],
    },
}

for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    method, msg_id = msg.get("method"), msg.get("id")
    log({"recv": msg})
    if method == "initialize":
        version = msg.get("params", {}).get("protocolVersion", "2025-06-18")
        reply(msg_id, {"protocolVersion": version, "capabilities": {"tools": {}},
                       "serverInfo": {"name": "amiri-toy", "version": "0"}})
    elif method == "tools/list":
        reply(msg_id, {"tools": [TOOL]})
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        decision = {"behavior": "allow", "updatedInput": args.get("input", {})}
        reply(msg_id, {"content": [{"type": "text", "text": json.dumps(decision)}]})
    elif msg_id is not None:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id,
                                     "error": {"code": -32601, "message": "method not found"}}) + "\n")
        sys.stdout.flush()
