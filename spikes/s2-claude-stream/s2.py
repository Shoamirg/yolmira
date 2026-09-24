"""Spike S2 (docs/ARCHITECTURE.md §16.2): Claude Code stream-json protocol.

Throwaway code. Drives `claude -p --input-format stream-json --output-format stream-json`
through six scenarios and writes findings.json to stdout. Raw streams are saved per
scenario: originals to the scratch folder, sanitized copies to ./fixtures.

Limits (docs/decisions.md): cheap model, low --max-turns, --max-budget-usd per process,
scratch cwd outside the repo, minimal tools, --safe-mode (no user hooks/plugins/MCP),
allowlisted environment, non-admin session. Aborts once total cost passes TOTAL_BUDGET_USD.

Usage: python s2.py <scratch-dir> [scenario ...] > findings.json  (no scenarios = all)
       python s2.py --resanitize <scratch-dir> ...
"""
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
import uuid
import winreg
from pathlib import Path

MODEL = "haiku"
PER_PROCESS_BUDGET = "0.15"
TOTAL_BUDGET_USD = 1.00
WAIT = 120
HERE = Path(__file__).resolve().parent
# Full path: CreateProcess searches the parent's PATH, not the child's allowlisted one.
CLAUDE = shutil.which("claude") or "claude"
FIXTURES = HERE / "fixtures"
POWERSHELL = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32/WindowsPowerShell/v1.0/powershell.exe"
PASS_THROUGH = [
    "SystemRoot", "windir", "SystemDrive", "ComSpec", "PATHEXT", "OS", "USERPROFILE", "HOMEDRIVE",
    "HOMEPATH", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERNAME", "USERDOMAIN", "ProgramData",
    "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "CommonProgramFiles", "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
]


class BudgetExceeded(Exception):
    pass


def registry_path():
    """Fresh PATH from the registry (machine + user), variables expanded."""
    parts = []
    for root, key in ((winreg.HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
                      (winreg.HKEY_CURRENT_USER, r"Environment")):
        try:
            with winreg.OpenKey(root, key) as k:
                parts.append(winreg.ExpandEnvironmentStrings(winreg.QueryValueEx(k, "Path")[0]))
        except OSError:
            pass
    return ";".join(p.strip(";") for p in parts if p)


def allowlist_env():
    """The decided child environment: fresh Windows values only, nothing else inherited."""
    env = {k: os.environ[k] for k in PASS_THROUGH if k in os.environ}
    env["PATH"] = registry_path()
    return env


def process_table():
    """pid -> (ppid, name), via CIM (locale-neutral numbers and image names)."""
    cmd = "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress"
    out = subprocess.run([str(POWERSHELL), "-NoProfile", "-Command", cmd], capture_output=True, text=True).stdout
    return {p["ProcessId"]: (p["ParentProcessId"], p["Name"]) for p in json.loads(out or "[]")}


def descendants(root_pid, table):
    found, frontier = {}, [root_pid]
    while frontier:
        parent = frontier.pop()
        for pid, (ppid, name) in table.items():
            if ppid == parent and pid not in found and pid != root_pid:
                found[pid] = name
                frontier.append(pid)
    return found


class Claude:
    """One claude process speaking stream-json; records every line with a timestamp."""

    def __init__(self, name, cwd, args, on_permission=None, safe_mode=True):
        self.name, self.t0, self.lines, self.stderr = name, time.monotonic(), [], []
        self.events, self.on_permission, self.req = queue.Queue(), on_permission, 0
        cmd = [CLAUDE, "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
               "--model", MODEL, "--max-budget-usd", PER_PROCESS_BUDGET, *(["--safe-mode"] if safe_mode else []),
               *args]
        self.cmd = cmd
        self.proc = subprocess.Popen(cmd, cwd=cwd, env=allowlist_env(), stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._read_err, daemon=True).start()

    def ms(self):
        return int((time.monotonic() - self.t0) * 1000)

    def _read(self):
        for raw in self.proc.stdout:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            self.lines.append({"ms": self.ms(), "dir": "out", "line": line})
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            req = ev.get("request", {}) if ev.get("type") == "control_request" else {}
            if req.get("subtype") == "can_use_tool" and self.on_permission:
                decision = self.on_permission(req)
                self.send({"type": "control_response", "response": {
                    "subtype": "success", "request_id": ev.get("request_id"), "response": decision}})
            self.events.put(ev)
        self.events.put({"type": "_eof"})

    def _read_err(self):
        for raw in self.proc.stderr:
            self.stderr.append({"ms": self.ms(), "line": raw.decode("utf-8", "replace").rstrip()})

    def send(self, obj):
        line = json.dumps(obj)
        self.lines.append({"ms": self.ms(), "dir": "in", "line": line})
        self.proc.stdin.write((line + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def user(self, text):
        self.send({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]},
                   "parent_tool_use_id": None, "session_id": "default"})

    def control(self, subtype, **extra):
        self.req += 1
        rid = f"amiri_{self.req}"
        self.send({"type": "control_request", "request_id": rid, "request": {"subtype": subtype, **extra}})
        return rid

    def wait_for(self, pred, timeout=WAIT):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                ev = self.events.get(timeout=max(0.1, deadline - time.monotonic()))
            except queue.Empty:
                break
            if ev.get("type") == "_eof":
                return None
            if pred(ev):
                return ev
        return None

    def result(self, timeout=WAIT):
        return self.wait_for(lambda e: e.get("type") == "result", timeout)

    def close_stdin(self):
        self.lines.append({"ms": self.ms(), "dir": "in", "line": "<stdin closed>"})
        self.proc.stdin.close()

    def wait_exit(self, timeout):
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()


class Run:
    def __init__(self, scratch):
        self.scratch, self.cost, self.procs = Path(scratch), 0.0, []
        self.home = os.environ.get("USERPROFILE", "")

    def workdir(self, name):
        d = self.scratch / "work" / name
        d.mkdir(parents=True, exist_ok=True)
        return d

    def start(self, name, args, **kw):
        c = Claude(name, self.workdir(name), args, **kw)
        self.procs.append(c)
        return c

    def account(self, ev):
        if ev and ev.get("type") == "result":
            self.cost += float(ev.get("total_cost_usd") or 0)
            if self.cost > TOTAL_BUDGET_USD:
                raise BudgetExceeded(f"total cost {self.cost:.4f} > {TOTAL_BUDGET_USD}")
        return ev

    def sanitize(self, text):
        for needle, label in ((str(self.scratch), "<SCRATCH>"), (self.home, "<HOME>")):
            if needle:
                # Fixture lines are JSON inside JSON, so backslashes appear 1x, 2x or 4x.
                forms = [needle.replace("\\", "\\" * n) for n in (4, 2, 1)] + [needle.replace("\\", "/")]
                for form in forms:
                    text = text.replace(form, label)
        for secret in filter(None, [os.environ.get("USERNAME"), os.environ.get("COMPUTERNAME")]):
            if len(secret) > 4:
                text = text.replace(secret, "<NAME>")
        return text

    def save(self, c):
        raw = self.scratch / "raw"
        raw.mkdir(parents=True, exist_ok=True)
        FIXTURES.mkdir(exist_ok=True)
        body = "\n".join(json.dumps(l, ensure_ascii=False) for l in c.lines) + "\n"
        (raw / f"{c.name}.jsonl").write_text(body, encoding="utf-8")
        (FIXTURES / f"{c.name}.jsonl").write_text(self.sanitize(body), encoding="utf-8")
        if c.stderr:
            (raw / f"{c.name}.stderr.jsonl").write_text(
                "\n".join(json.dumps(l, ensure_ascii=False) for l in c.stderr), encoding="utf-8")


def summarize(ev):
    """The parts of a result event that matter for the adapter."""
    if not ev:
        return None
    keep = ("subtype", "is_error", "num_turns", "session_id", "total_cost_usd", "stop_reason", "result",
            "permission_denials", "terminal_reason")
    return {k: ev.get(k) for k in keep if k in ev}


def s1_multiturn_and_idle_close(run, sid):
    c = run.start("s1_multiturn", ["--session-id", sid, "--tools", "", "--max-turns", "2"])
    init = c.wait_for(lambda e: e.get("type") == "system" and e.get("subtype") == "init", 60)
    c.user("Reply with exactly this token and nothing else: AMIRI_S2_ONE")
    r1 = run.account(c.result())
    c.user("Reply with exactly this token and nothing else: AMIRI_S2_TWO")
    r2 = run.account(c.result())
    closed = time.monotonic()
    c.close_stdin()
    code = c.wait_exit(30)
    out = {"init_seen_before_first_user_message": init is not None,
           "init_keys": sorted(init.keys()) if init else None,
           "init_session_id_matches": bool(init) and init.get("session_id") == sid,
           "turn1": summarize(r1), "turn2": summarize(r2),
           "idle_stdin_close_exit_code": code,
           "idle_stdin_close_exit_ms": int((time.monotonic() - closed) * 1000) if code is not None else None}
    c.stop()
    run.save(c)
    return out


def s2_resume(run, sid):
    c = run.start("s2_resume", ["--resume", sid, "--tools", "", "--max-turns", "1"])
    c.user("What was the first token you replied with in this conversation? Reply with that token only.")
    r = run.account(c.result())
    c.close_stdin()
    code = c.wait_exit(30)
    c.stop()
    run.save(c)
    return {"result": summarize(r), "remembers_first_turn": bool(r) and "AMIRI_S2_ONE" in str(r.get("result")),
            "same_session_id": bool(r) and r.get("session_id") == sid, "exit_code": code}


def s3_permission_stdio(run):
    seen = []

    def decide(req):
        seen.append({k: req.get(k) for k in ("tool_name", "input", "tool_use_id", "permission_suggestions")})
        path = str(req.get("input", {}).get("file_path", ""))
        if path.endswith("s2_denied.txt"):
            return {"behavior": "deny", "message": "AMIRI policy: this write is denied."}
        return {"behavior": "allow", "updatedInput": req.get("input", {})}

    c = run.start("s3_permission_stdio", ["--tools", "Write", "--permission-prompt-tool", "stdio",
                                          "--max-turns", "4"], on_permission=decide)
    init_id = c.control("initialize", hooks=None)
    init_resp = c.wait_for(lambda e: e.get("type") == "control_response", 30)
    c.user("Use the Write tool to create the file s2_allowed.txt in the current directory "
           "containing exactly AMIRI_S2_FILE. Then reply DONE.")
    r1 = run.account(c.result())
    c.user("Use the Write tool to create the file s2_denied.txt in the current directory "
           "containing exactly AMIRI_S2_NO. If it is denied, reply DENIED.")
    r2 = run.account(c.result())
    c.close_stdin()
    c.wait_exit(30)
    c.stop()
    run.save(c)
    wd = run.workdir("s3_permission_stdio")
    allowed = wd / "s2_allowed.txt"
    return {"initialize_request_id": init_id,
            "initialize_response_subtype": (init_resp or {}).get("response", {}).get("subtype"),
            "permission_requests": seen,
            "allowed_file_written": allowed.exists() and "AMIRI_S2_FILE" in allowed.read_text(encoding="utf-8"),
            "denied_file_absent": not (wd / "s2_denied.txt").exists(),
            "turn_allow": summarize(r1), "turn_deny": summarize(r2)}


def s4_permission_mcp(run, safe_mode):
    name = f"s4_permission_mcp_{'safe' if safe_mode else 'nosafe'}"
    wd = run.workdir(name)
    mcp_log = wd.parent / f"{name}.toy_mcp.log"
    cfg = {"mcpServers": {"amiri": {"command": sys.executable, "args": [str(HERE / "toy_mcp.py"), str(mcp_log)]}}}
    extra = [] if safe_mode else ["--setting-sources", "local"]
    c = run.start(name, ["--tools", "Write", "--mcp-config", json.dumps(cfg), "--strict-mcp-config",
                         "--permission-prompt-tool", "mcp__amiri__approve", "--max-turns", "3", *extra],
                  safe_mode=safe_mode)
    c.user("Use the Write tool to create the file s2_mcp.txt in the current directory containing exactly "
           "AMIRI_S2_MCP. Then reply DONE.")
    r = run.account(c.result())
    c.close_stdin()
    c.wait_exit(30)
    c.stop()
    run.save(c)
    calls = []
    if mcp_log.exists():
        for line in mcp_log.read_text(encoding="utf-8").splitlines():
            msg = json.loads(line).get("recv", {})
            if msg.get("method") == "tools/call":
                calls.append(msg.get("params"))
    f = wd / "s2_mcp.txt"
    return {"safe_mode": safe_mode, "approve_tool_calls": calls,
            "file_written": f.exists() and "AMIRI_S2_MCP" in f.read_text(encoding="utf-8"),
            "result": summarize(r)}


def s5_interrupt(run):
    c = run.start("s5_interrupt", ["--tools", "", "--include-partial-messages", "--max-turns", "2"])
    c.user("Write the numbers from 1 to 400, one per line, with no other text.")
    first = c.wait_for(lambda e: e.get("type") == "stream_event", 60)
    time.sleep(1.0)
    rid = c.control("interrupt")
    ack = c.wait_for(lambda e: e.get("type") == "control_response", 30)
    r1 = run.account(c.result(60))
    c.user("Reply with exactly this token and nothing else: AMIRI_S2_AFTER")
    r2 = run.account(c.result())
    c.close_stdin()
    c.wait_exit(30)
    c.stop()
    run.save(c)
    return {"saw_partial_before_interrupt": first is not None, "interrupt_request_id": rid,
            "interrupt_ack": (ack or {}).get("response"), "interrupted_turn": summarize(r1),
            "next_turn": summarize(r2),
            "session_usable_after_interrupt": bool(r2) and "AMIRI_S2_AFTER" in str(r2.get("result"))}


def s6_stdin_close_mid_turn(run):
    c = run.start("s6_stdin_close", ["--tools", "Bash", "--allowedTools", "Bash(ping *)", "--max-turns", "2"])
    c.user("Use the Bash tool to run exactly this command and wait for it: ping -n 40 127.0.0.1 "
           "Then reply DONE.")
    tool = c.wait_for(lambda e: e.get("type") == "assistant" and any(
        b.get("type") == "tool_use" for b in e.get("message", {}).get("content", [])), 90)
    time.sleep(4)
    tree = descendants(c.proc.pid, process_table())
    closed = time.monotonic()
    c.close_stdin()
    code = c.wait_exit(15)
    exit_ms = int((time.monotonic() - closed) * 1000) if code is not None else None
    time.sleep(3)
    t3 = process_table()
    alive_3s = {str(p): n for p, n in tree.items() if p in t3 and t3[p][1] == n}
    time.sleep(7)
    t10 = process_table()
    alive_10s = {str(p): n for p, n in tree.items() if p in t10 and t10[p][1] == n}
    claude_alive_10s = c.proc.poll() is None
    for pid in tree:  # cleanup whatever is left
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)
    c.stop()
    run.save(c)
    return {"tool_use_seen": tool is not None,
            "descendants_before_close": {str(p): n for p, n in tree.items()},
            "claude_exit_code_after_close": code, "claude_exit_ms_after_close": exit_ms,
            "claude_alive_10s_after_close": claude_alive_10s,
            "descendants_alive_3s_after": alive_3s, "descendants_alive_10s_after": alive_10s}


def s7_stdin_close_then_natural_end(run):
    """Like s6, but nothing is killed: does claude exit once the running turn ends?"""
    c = run.start("s7_stdin_close_natural", ["--tools", "Bash", "--allowedTools", "Bash(ping *)",
                                             "--max-turns", "2"])
    c.user("Use the Bash tool to run exactly this command and wait for it: ping -n 8 127.0.0.1 "
           "Then reply DONE.")
    tool = c.wait_for(lambda e: e.get("type") == "assistant" and any(
        b.get("type") == "tool_use" for b in e.get("message", {}).get("content", [])), 90)
    time.sleep(2)
    tree = descendants(c.proc.pid, process_table())
    closed = time.monotonic()
    c.close_stdin()
    r = run.account(c.result(90))
    code = c.wait_exit(60)
    exit_ms = int((time.monotonic() - closed) * 1000) if code is not None else None
    t = process_table()
    left = {str(p): n for p, n in tree.items() if p in t and t[p][1] == n}
    for pid in tree:
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)
    c.stop()
    run.save(c)
    return {"tool_use_seen": tool is not None, "descendants_before_close": {str(p): n for p, n in tree.items()},
            "result_after_close": summarize(r), "claude_exit_code": code, "claude_exit_ms_after_close": exit_ms,
            "descendants_left_after_exit": left}


def git_state():
    def git(*args):
        r = subprocess.run(["git", "-C", str(HERE), *args], capture_output=True, text=True)
        return r.stdout.strip() if r.returncode == 0 else None
    dirty = git("status", "--porcelain", "--untracked-files=no", "--", ":/", ":(exclude)findings.json",
                ":(exclude)fixtures")
    return {"commit": git("rev-parse", "HEAD"), "dirty_tracked_files": None if dirty is None else bool(dirty)}


def resanitize(scratch):
    """Rewrite ./fixtures from a run's raw copies with the current sanitizer (no API calls)."""
    run = Run(scratch)
    for raw in sorted((Path(scratch) / "raw").glob("*.jsonl")):
        if not raw.name.endswith(".stderr.jsonl"):
            (FIXTURES / raw.name).write_text(run.sanitize(raw.read_text(encoding="utf-8")), encoding="utf-8")


def main():
    if sys.argv[1] == "--resanitize":
        for scratch in sys.argv[2:]:
            resanitize(scratch)
        return
    run = Run(sys.argv[1])
    sid = str(uuid.uuid4())
    version = subprocess.run([CLAUDE, "--version"], capture_output=True, text=True, env=allowlist_env())
    report = {"spike": "S2-claude-stream", "git": git_state(), "claude_version": version.stdout.strip(),
              "model_alias": MODEL, "per_process_budget_usd": PER_PROCESS_BUDGET, "scenarios": {}}
    only = set(sys.argv[2:])
    steps = [("s1_multiturn_and_idle_close", lambda: s1_multiturn_and_idle_close(run, sid)),
             ("s2_resume", lambda: s2_resume(run, sid)),
             ("s3_permission_stdio", lambda: s3_permission_stdio(run)),
             ("s4_permission_mcp_safe", lambda: s4_permission_mcp(run, True)),
             ("s5_interrupt", lambda: s5_interrupt(run)),
             ("s6_stdin_close_mid_turn", lambda: s6_stdin_close_mid_turn(run)),
             ("s7_stdin_close_then_natural_end", lambda: s7_stdin_close_then_natural_end(run))]
    for name, step in [s for s in steps if not only or s[0] in only]:
        try:
            report["scenarios"][name] = step()
        except BudgetExceeded as e:
            report["scenarios"][name] = {"error": str(e)}
            break
        except Exception as e:  # record and continue: each scenario is independent
            report["scenarios"][name] = {"error": f"{type(e).__name__}: {e}"}
        finally:
            for c in run.procs:
                c.stop()
        if name == "s4_permission_mcp_safe" and not report["scenarios"][name].get("approve_tool_calls"):
            try:  # --safe-mode may also disable --mcp-config servers; retry without it
                report["scenarios"]["s4_permission_mcp_nosafe"] = s4_permission_mcp(run, False)
            except Exception as e:
                report["scenarios"]["s4_permission_mcp_nosafe"] = {"error": f"{type(e).__name__}: {e}"}
            finally:
                for c in run.procs:
                    c.stop()
        print(f"{name}: done (cost so far {run.cost:.4f})", file=sys.stderr)
    report["total_cost_usd"] = round(run.cost, 4)
    print(run.sanitize(json.dumps(report, indent=2, ensure_ascii=False)))


if __name__ == "__main__":
    main()
