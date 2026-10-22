# Spike S1c — WSL processes under a kill-on-close job: findings

- Date: 2026-09-24
- Raw evidence: `findings.json`: clean commit `9eacfd8`, `"elevated": false` (Medium integrity), exit 0. Distro Ubuntu-22.04 (WSL 2.7.14, tmux 3.2a).
- Method: a host process (stands in for `amirid`) joins a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then starts two things through `wsl.exe` (both `wsl.exe` processes are in the job): a foreground `sleep 607`, and `tmux new-session -d -s amiri_s1c 'sleep 613'`. The parent kills the host with `TerminateProcess` and checks the Linux side from outside the job with `pgrep -fx` / `tmux has-session` (fixed tokens only, locale-neutral). The spike cleans up only its own session and markers. No tokens spent.

## Observations

| Linux process | Before kill | 3 s after | 10 s after |
|---|---|---|---|
| Foreground `sleep 607` (its `wsl.exe` in the job) | alive | **dead** (`wsl.exe` dead too) | dead |
| tmux session `amiri_s1c` | alive | **alive** | **alive** |
| `sleep 613` inside that tmux session | alive | **alive** | **alive** |

## Answers

1. **The job reaches foreground WSL processes, but only through `wsl.exe`.** Killing the job kills `wsl.exe`, and the Linux process attached to it dies with it. So an agent run as `wsl.exe -d <distro> -- claude -p …` in the foreground *is* contained by §9.6. Caveat: tested with `sleep`, which does not detach. A Linux program that daemonizes itself (`setsid`, double fork, `nohup … &`) leaves `wsl.exe`'s reach, exactly like tmux below.
2. **The tmux server survives even though the `wsl.exe` that started it was in the job.** tmux daemonizes inside the Linux VM, and the Windows job never sees Linux processes. So §9.6's "killing a `wsl.exe` client must not take down the tmux server" holds **without** any special handling: there's no need to keep WSL clients out of the job for tmux persistence.
3. **Consequence: work started inside WSL tmux is not contained by the job.** An agent launched in a tmux session survives an `amirid` crash. That is the intended persistence for long-running work (§17 Q10), but it means startup reconciliation must also list AMIRI's tmux sessions in each WSL distro (by a fixed name prefix) and report them, the same "report, never kill silently" rule as for orphaned Windows PIDs.

## Not covered

- An agent CLI (instead of `sleep`) as the foreground process, and whether it spawns detached helpers inside WSL. Folded into S2/S3 when run under WSL.
- `wsl --shutdown` / distro termination while `amirid` is attached.
