# Spike S1b — child lifetime when the host dies: findings

- Date: 2026-09-24
- Raw evidence: `findings.json`: run on clean commit `8e98c8b` (no tracked changes), exit 0, 3/3 scenarios
- Caveat: run **elevated** (`"elevated": true`, measured by the prototype token check). Rerun from a non-admin session.
- Method: a host process (stands in for `amirid`) spawns a fake agent (this binary in `child` mode), then the parent kills the host with `TerminateProcess` (≈ daemon crash). The child logs console control events, stdin EOF and stdout write errors with timestamps; job membership is queried with `IsProcessInJob`. No tokens spent.

## Observations

| Scenario | Child alive 3 s / 10 s after host kill | What the child saw (ms after kill) | In a job object? |
|---|---|---|---|
| ConPTY child (ignores EOF) | **no / no** | `+30 CTRL_EVENT 2` (CTRL_CLOSE_EVENT), then terminated | no (child, host, parent) |
| Pipe child that exits on stdin EOF | **no / no** | `+31 STDIN_EOF` → exits | no |
| Pipe child that ignores stdin EOF | **yes / yes: orphaned** | `+34 STDIN_EOF`, `+69 STDOUT_ERR (os error 232)`, then kept running ≥ 10.8 s until the spike killed it | no |

## Answers

1. **Why the S1 shell died: the console closed, not a job object.** No process in the chain was in a job. The ConPTY child received `CTRL_CLOSE_EVENT` 30 ms after the host died and was terminated, although its handler returned TRUE (Windows ends the process after a close event regardless). The pseudoconsole's lifetime follows the process that owns it.
   → **Consequence for Q10:** a separate `amiri-sessiond` that owns the ConPTYs *would* keep local shells alive across an `amirid` crash, because nothing else kills them. The option is technically sound; whether it's worth building stays deferred (§17 Q10) until there's data on crash frequency.

2. **Structured (pipe) children don't die on their own; they die only if they react to EOF.** Killing the host closes the child's stdin (EOF within ~30 ms) and breaks its stdout (`os error 232`, ERROR_NO_DATA). A child that exits on EOF is gone at once. A child that doesn't **keeps running as an orphan**: no supervisor, output going nowhere, and possibly still executing tools.
   → Whether real `claude -p` and `codex app-server` exit on EOF is **still unknown**. S2 and S3 must test it (close stdin mid-turn and see whether the process tree survives).
   → **Recommendation `[DECISION proposed]`:** `amirid` assigns every agent process it spawns to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. When `amirid` dies, the kernel closes the job handle and kills the whole agent process tree deterministically, whatever the CLI does on EOF. This matches the "kill the process tree by job object" in §9.3 but extends it from `terminate()` to crash. Conversations still resume from the provider's session store (§D3). The ConPTY/shell processes are *not* put in that job if `amiri-sessiond` is ever built.
   → On restart, reconciliation also looks for surviving processes recorded by the previous run (PID + creation time, to avoid PID reuse) and reports them as orphans instead of assuming they're gone.

3. **Error text is localized even for OS errors**: `os error 232` came back as `Идет закрытие канала.` Classify I/O failures by OS error code (`raw_os_error()`), never by message (§15 locale rule).

4. The elevation check (`OpenProcessToken` + `TokenElevation`) works and correctly reported this session as elevated; it can be lifted into `amirid` startup (§6.5).
