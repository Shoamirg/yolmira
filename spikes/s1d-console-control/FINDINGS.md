# Spike S1d — amirid console control and child Ctrl+C: findings

- Date: 2026-09-24
- Raw evidence: `findings.json`: clean commit `b42c776`, non-admin session, exit 0, 3/3 modes ran.
- Method: the parent sets the ignore-Ctrl+C flag on itself (what a `CREATE_NEW_PROCESS_GROUP` launcher does), so every host starts from the S1 failure condition regardless of how the spike was launched. A host (stands in for `amirid`) runs inside an outer ConPTY; it opens an inner ConPTY with `cmd.exe`, runs `ping -t`, sends `\x03` and checks that the prompt returns. The parent then sends `\x03` to the host's own console, checks that the host is still running 2 s later, and closes the outer pseudoconsole. Events go to a log file as fixed tokens (locale-neutral). No tokens spent.

## Observations

| Host mode | Inner shell interrupted by Ctrl+C | Host survives Ctrl+C in its own console | Clean shutdown on console close |
|---|---|---|---|
| `none` (inherited flag left alone) | **no** (prompt never returned) | yes | no: killed, exit `0xC000013A` |
| `restore-only` (`SetConsoleCtrlHandler(NULL, FALSE)`) | yes | **no**: died on the Ctrl+C, exit `0xC000013A` | n/a (already dead) |
| `fixed` (restore + own handler) | **yes** | **yes** (`HOST_IGNORED_CTRL 0`) | **yes** (`HOST_CLEAN_SHUTDOWN 2`, exit 0) |

`0xC000013A` is `STATUS_CONTROL_C_EXIT`. Event numbers: 0 = `CTRL_C_EVENT`, 2 = `CTRL_CLOSE_EVENT`.

## Answers

1. **The approved design works as specified** (decisions.md, "Ctrl+C in shells"). Clearing the inherited flag fixes Ctrl+C in shells; the handler keeps `amirid` alive on Ctrl+C and turns console close into a clean shutdown with exit 0.
2. **The reviewer's concern was real:** clearing the flag alone (`restore-only`) makes a Ctrl+C in `amirid`'s own terminal kill the daemon and every session with it.
3. **The two mechanisms are independent,** as expected: the handler lives only in `amirid`; the flag state (cleared) is what the children inherit. Installing the handler did not stop the inner shell from getting Ctrl+C.
4. Console close gives the handler a few seconds (Windows kills the process after the handler returns or times out); `amirid`'s clean shutdown on `CTRL_CLOSE_EVENT` must fit in that window: flush the event log, mark sessions, and exit. Anything longer belongs to a normal shutdown path.
