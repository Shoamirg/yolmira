# Spike S2 — Claude Code stream-json protocol: findings

- Date: 2026-09-24. Claude Code 2.1.281 (native `claude.exe`), model alias `haiku` → `claude-haiku-4-5-20251001`.
- Raw evidence: `findings.json` (scenarios 1–6, clean commit `7d358e3`) and `findings-s7.json` (scenario 7, clean commit `6e63f7b`). Sanitized streams in `fixtures/*.jsonl` (every line in and out, with ms timestamps; local paths replaced by `<SCRATCH>`/`<HOME>`). Fixtures were re-sanitized from the raw copies at `c49fe9a` without re-running.
- Limits held (decisions.md): non-admin session, `--safe-mode` (no user hooks, plugins, rules or MCP), minimal `--tools`, `--max-turns` ≤ 4, `--max-budget-usd 0.15` per process, $1 total cap, scratch folder under `%TEMP%` (not the repo), **allowlisted environment** (registry PATH + standard Windows variables only). **Total spend: $0.089.**
- Side effect: the runs left session transcripts under `~/.claude/projects/C--Users-User-AppData-Local-Temp-amiri-s2-*`. Not deleted (the user's session store); safe to remove by hand.

## Observations

| # | Scenario | Result |
|---|---|---|
| 1 | Two turns in one process, then close stdin while idle | Both turns answered in the same session. `--session-id <uuid>` is honoured. The `system/init` event is emitted **after the first user message**, not at spawn, and again on later turns. Idle stdin close → **exit 0 in 594 ms**. |
| 2 | New process with `--resume <uuid>` | Same session id; the model recalled turn 1's token. Exit 0. |
| 3 | Permission prompts over stdio (`--permission-prompt-tool stdio`) | Host sent `control_request {subtype: initialize}` → `success`. For each Write the CLI sent `control_request {subtype: can_use_tool, tool_name, input, tool_use_id, permission_suggestions}`. Reply `control_response {subtype: success, request_id, response: {behavior: "allow", updatedInput}}` → file written. Reply `{behavior: "deny", message}` → file **not** written, the model saw the denial and answered `DENIED`, and the turn's `result.permission_denials` lists the denied call. |
| 4 | Permission prompts through a toy MCP server (`--permission-prompt-tool mcp__amiri__approve`) | **Without** `--safe-mode`: works; the server receives `tools/call approve {tool_name, input, tool_use_id}` and the allow decision as JSON text. **With** `--safe-mode`: `--mcp-config` servers are disabled too (`mcp_servers: []`), the permission tool is "not found", and the Write fails as a tool error. It fails closed, but no `result` event followed. |
| 5 | Interrupt mid-answer (`control_request {subtype: interrupt}`) | Ack `control_response success {still_queued: []}`. The turn ends with `result {subtype: error_during_execution, is_error: true, terminal_reason: aborted_streaming}` and **`total_cost_usd: 0`** although tokens were streamed. The next user message in the same process works normally. |
| 6 | Close stdin mid-turn while Bash runs `ping -n 40` | `claude` **kept running**: no exit within 15 s, still alive at 10 s, and its whole tree (`bash.exe` ×3, `conhost.exe`, `PING.EXE`) alive. The tool only ended when the harness killed `ping` (task `failed`). |
| 7 | Same, with `ping -n 8`, nothing killed | After stdin EOF, `claude` **finished the turn** (tool, then another model call), emitted a normal `result` (`DONE`), and **exited 0 after 7.75 s** with no processes left. |

## Answers for the Claude adapter (§9)

1. **Transport:** one long-lived `claude -p --input-format stream-json --output-format stream-json --verbose` per conversation works, multi-turn. Resume across processes via `--session-id` / `--resume` works. Don't wait for `init` before sending the first message: it only arrives after it.
2. **Permissions: use the stdio control protocol, not an MCP tool.** `--permission-prompt-tool stdio` puts every permission decision on the same pipe as the conversation, needs no extra server process, works under `--safe-mode`, and gives the Policy Engine `tool_name`, full `input` and `tool_use_id`. Deny carries a message the model sees. `[DECISION proposed]`.
   `--safe-mode` looks like the right default for AMIRI-launched agents (no user hooks or plugins firing inside orchestrated runs), but it also disables `--mcp-config`. If AMIRI later needs its own MCP servers in a session, that session runs without `--safe-mode` and with `--setting-sources` limited instead. `[DECISION proposed]`.
3. **Interrupt works and the session survives it.** But an interrupted turn reports `total_cost_usd: 0`. AMIRI's cost tracking must not rely on `result.total_cost_usd` for aborted turns (use the per-message `usage` in the stream, or mark the turn's cost unknown).
4. **Stdin EOF is not a stop signal.** Closing stdin mid-turn lets the current turn run to completion: tools keep executing and the model is called again. Only then does `claude` exit cleanly (0, no orphans). Consequences:
   - To stop work, AMIRI sends `interrupt` (graceful) or terminates the job (§9.6). Closing stdin is only a clean way to end an **idle** session.
   - On an `amirid` crash, `claude` would otherwise finish its turn unsupervised, still executing tools with nobody to answer permission prompts. That confirms the kill-on-close job (§9.6) for Claude. The tree to contain is `claude.exe` → Git Bash (`bash.exe` ×3 + `conhost.exe`) → the command.
5. **The allowlisted environment is enough.** `claude` authenticated with its own login (`apiKeySource: "none"`), found Git Bash, and resolved PowerShell by full path (`init.powershell_path`) although this machine's PATH lacks it.
6. **Version gating:** `init.capabilities` lists protocol features (`interrupt_receipt_v1`, `interrupt_cancel_queued_v1`, `msg_lifecycle_v1`, …). The adapter should check these instead of parsing `claude_code_version`.
7. **Other stream events to handle or ignore:** `system/thinking_tokens`, `rate_limit_event` (includes 5-hour and 7-day utilization; worth surfacing in the UI), `system/task_started` / `task_notification`, `tool_progress` heartbeats every 30 s during long tools.

## Not covered

- `claude` under WSL (`wsl.exe -- claude -p …`); S1c covers the process side.
- `--permission-prompts none` (auto-deny) and `permission_suggestions` beyond `setMode`.
- Kill-on-close job against the real `claude` tree (the mechanism is proven in S1b/S1c; worth one check in Phase 1's process-manager tests).
