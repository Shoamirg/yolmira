# History rewrite (2026-10-22)

Before the repository went public (decisions.md 2026-10-22), `git filter-repo` rewrote every commit:

- the author and committer email became the GitHub noreply address;
- old versions of `spikes/s2-claude-stream/fixtures/*.jsonl` had the Claude account email and plan replaced with `<EMAIL>` / `<PLAN>`, as the 2026-09-25 scrub did for the current files.

The newest tree is byte-identical to the one before the rewrite, so every commit's files are unchanged except those fixture lines. Commit messages had their cited hashes updated by `filter-repo`. Markdown docs and `.gitleaksignore` were remapped in the commit after the rewrite. JSON evidence (`findings*.json`, which record the commit a spike ran on) and the file name `findings-6ebeda6-superseded.json` keep the hash as recorded: look it up here.

| Old | New | Date | Commit |
|---|---|---|---|
| `ee70141` | `1a4e51d` | 2026-10-09 | test: the soak reports its client's resyncs and the output budgets' high-water marks |
| `240bbe9` | `976a589` | 2026-10-09 | docs: output-budget fix results; open finding - one daemon crash (0xC0000409) in 4 suite runs, now captured if it recurs |
| `db112bc` | `e3920c5` | 2026-10-09 | fix: a detached daemon's stderr and panics go to logs\yolmirad-stderr.log, written synchronously; failing tests keep their home |
| `c86846c` | `6210273` | 2026-10-09 | test: budget floods run one at a time and write 200-char lines, so the slow client always falls behind |
| `fa08596` | `bfad4bf` | 2026-10-09 | test: budget tests run as the trusted test UI (many connections from one process); comparable-output threshold matches a 15 s flood |
| `b1ce605` | `2ccd493` | 2026-10-09 | fix: bound daemon memory per client - byte budgets, resync, stall and connection limits |
| `384cde5` | `4621675` | 2026-10-09 | docs: Phase 1 exit checks - Yolmira E2E and WSL results, overload-path soak, memory breakdown |
| `f137eb8` | `28115dc` | 2026-10-09 | test: memory_breakdown adds a slow-reading client during a flood |
| `9f1b5bc` | `9f7fd09` | 2026-10-09 | test: memory_breakdown diagnostic - yolmirad private bytes per stage (gated by YOLMIRA_MEMPROBE) |
| `9f9acb5` | `39f6c17` | 2026-10-09 | refactor!: rename AMIRI to Yolmira; app icon |
| `fc4d8df` | `9dff1e7` | 2026-10-09 | docs: Phase 1 exit checks - fresh-build E2E result, steps for logoff and second-account pipe-owner checks |
| `f697155` | `2201a04` | 2026-10-09 | test: soak checks the exit status still arrives after the flood |
| `92c87b9` | `dea6b49` | 2026-10-09 | docs: threat model until S7 - front-door misuse, not deliberate attack with OS tools |
| `f2273c4` | `98f017a` | 2026-10-09 | docs: config-file guard in Slice D findings, decisions and ARCHITECTURE; ipc tests expect the guard's first-start events |
| `0790840` | `d04c7e4` | 2026-10-09 | feat: loosening edits of policies.toml / agents.toml wait for a person |
| `335efc0` | `0399bd2` | 2026-10-09 | docs: Phase 1 Slice D findings - exit tests, approvals vs agents, deviations, open items |
| `b0f1cbb` | `cd74cb4` | 2026-10-09 | feat(ui): project picker - register a folder, new shells start in the chosen project |
| `b6e90c7` | `771a0a2` | 2026-10-09 | feat: amiri doctor v1 - daemon, pipe, database, sessions, UAC, agents as data |
| `7c46152` | `65c3ff0` | 2026-10-09 | feat: identity split into person_id (portable) and install_id (per machine) |
| `8e23295` | `8681d14` | 2026-10-09 | feat(ui): approval panel - allow once / for this run / deny, CLI code shown |
| `8521e76` | `2bdcfb6` | 2026-10-09 | feat: person-aware identity in events and approvals (team mode, no later migration) |
| `255725c` | `0551335` | 2026-10-09 | docs: team mode proposal (after Phase 8); identity becomes person-aware in Slice D |
| `75f5a7d` | `62057a9` | 2026-10-09 | docs: Slice C stale-run audit and fresh rerun; R14 + spike S7; proposals |
| `ab2467d` | `e9845ea` | 2026-10-09 | fix(test): freshness guard ignores tests/ folders (they don't change the binaries) |
| `74a4a7d` | `02a48d7` | 2026-10-09 | test: every_mutating_method_emits_event walks the protocol method table |
| `9a7029c` | `6c5565f` | 2026-10-09 | fix: trusted test UI via a config file (the daemon's environment is an allowlist) |
| `5fbaf3a` | `ec15372` | 2026-10-09 | test: ask_round_trip checks the order of the kill's own events |
| `0050eda` | `ffe7144` | 2026-10-09 | feat: policy gate wired in; approvals need the UI, never an agent job |
| `76590d9` | `e82a027` | 2026-10-09 | feat: process peer helpers - job membership, image path, pipe client pid |
| `4329316` | `fc4ab4e` | 2026-10-09 | docs: repair Slice C after-acceptance section (text lost to shell quoting) |
| `6685a08` | `19a11e4` | 2026-10-09 | test: off-screen E2E windows by default; Slice C findings after acceptance |
| `3707c4e` | `b937db9` | 2026-10-09 | fix(test): freshness guard compares each binary with its own sources |
| `c0c210d` | `71e6b5d` | 2026-10-09 | fix(test): off-screen E2E windows are moved after creation |
| `488424f` | `dba628e` | 2026-10-09 | fix(test): E2E always rebuilds and refuses stale binaries |
| `7419ae6` | `4032899` | 2026-10-09 | chore: msedgedriver 154.0.4258.62 pinned (WebView2 updated itself) |
| `c9d5ae2` | `1a376bf` | 2026-10-09 | fix: ui.log records why every UI exited; off-screen option for E2E windows |
| `53f7739` | `359592f` | 2026-10-06 | feat: project registry in the store; sessions carry their project; connection ids |
| `2e0b8a9` | `74cf3d2` | 2026-10-05 | test: catch the UI disappearing - panic hook to ui.log, exit code and output in diagnostics |
| `c48a77c` | `8a3af4e` | 2026-10-05 | feat: policy engine core - typed actions, policies.toml rules, hard limits |
| `e1c942f` | `ad73935` | 2026-10-05 | docs: msedgedriver instead of tauri-driver; acceptance decisions; UI dependency rows |
| `829da87` | `decddfe` | 2026-10-05 | test: E2E failures report UI liveness and daemon/UI log tails |
| `41e8bc5` | `e61fa0e` | 2026-10-05 | fix: no stale RUNNING by design; bounded pty tests with dumps; E2E leftover check |
| `8059365` | `59576ea` | 2026-10-05 | docs: Slice C CI run and durations |
| `9acaed9` | `4e511bf` | 2026-10-05 | docs: Phase 1 Slice C findings - exit tests, review fixes, deviations, open items |
| `c7dafab` | `4821883` | 2026-10-05 | test: daemon-down test reports the rendered trail; anchored driver signer check |
| `9fce2ca` | `57b647f` | 2026-10-05 | fix: Slice C review findings - flow control, reconnect races, release env hooks |
| `86e7be9` | `2b73b07` | 2026-10-05 | test: e2e call sites pass track to Ui.launch |
| `54f0dde` | `f464f06` | 2026-10-05 | feat: Phase 1 Slice C (WIP) - Tauri UI, E2E harness, CI UI steps |
| `bd36f02` | `f5eb774` | 2026-10-05 | docs: propose replacing Codex with Gemini CLI and local Qwen |
| `4b5fae5` | `3ceeba4` | 2026-09-26 | chore: explicit line-ending rules; Phase 9 item to zeroize scrollback |
| `1b623e4` | `13adbc4` | 2026-09-26 | docs: Slice B findings - where scrollback lives, CI durations, WSL-terminate plan |
| `d20ee5d` | `dba3c3d` | 2026-09-25 | docs: record Phase 1 Slice B findings, test runs and CI |
| `5f3f30d` | `5226c24` | 2026-09-25 | ci: warm up Windows PowerShell; test steps allow a cold first command |
| `9a3e679` | `0d255b2` | 2026-09-25 | test: wait for the PowerShell prompt before typing |
| `7ae4df3` | `83ec2da` | 2026-09-25 | ci: print PSReadLine and OS versions; longer output tail in terminal test failures |
| `5a010b7` | `768bc57` | 2026-09-25 | feat: Phase 1 Slice B - terminal sessions from the CLI |
| `34e04a1` | `06cb40e` | 2026-09-25 | fix: amiri doctor suggests registry-safe fixes for ComSpec and PATH |
| `07e0f0e` | `1f18e3f` | 2026-09-25 | feat: amiri doctor checks ComSpec and PowerShell on PATH; simulated-machine tests |
| `8ff620f` | `27e0635` | 2026-09-25 | docs: record Phase 1 Slice A findings, test runs and CI |
| `9ccbd0d` | `8f2bfad` | 2026-09-25 | fix: CI runs every test it can inside the runner's restrictive job |
| `52ae01b` | `70332d3` | 2026-09-25 | feat: Phase 1 Slice A - daemon, named-pipe IPC, CLI and event log |
| `85fedb8` | `7e4dba9` | 2026-09-25 | chore: add gitleaksignore for the public Ubuntu signing-key fingerprint |
| `ffda53a` | `c5bebcd` | 2026-09-25 | fix: scrub the Claude account email and plan from S2 fixtures |
| `ada5364` | `c91bb5a` | 2026-09-25 | docs: approve Phase 1 plan without the WMI fallback; CI and download decisions |
| `0e3087d` | `458d048` | 2026-09-25 | docs: Phase 1 plan for review; record Claude Code auto-update to 2.1.282 |
| `c37e726` | `607d34e` | 2026-09-25 | docs: record permission route and safe-mode decisions; extend risk list |
| `ecfd5ad` | `4ce8bb6` | 2026-09-25 | docs: Phase 0 close-out; defer S3 to a Phase 4 gate; named risk list |
| `5ae0ac3` | `8703150` | 2026-09-25 | docs: gate automatic failover on a captured limit event; Compute is Phase 5b |
| `89c6a1a` | `e21779f` | 2026-09-25 | docs: usage-limit failover, provider account rule, compute proposal |
| `dd3b025` | `47fee5f` | 2026-09-25 | docs: approve tmux status off, add dependency register, tidy S4 findings |
| `e008516` | `6fa4ad3` | 2026-09-24 | docs: record S4 russh findings and teardown |
| `6660566` | `a00ad4f` | 2026-09-24 | fix: S4 checks fail when the observed value differs from the expected one |
| `6ebeda6` | `9c3b89a` | 2026-09-24 | fix: S4 creates the SFTP file explicitly and uses an unechoed resize marker |
| `4e56f04` | `eb595ab` | 2026-09-24 | test: add spike S4 russh client checks (ring backend) |
| `1eb2c23` | `468fddd` | 2026-09-24 | fix: S4 setup stops Git Bash rewriting the host key path |
| `05ec3a0` | `425ea88` | 2026-09-24 | fix: S4 scripts use Windows ssh-add and regenerate the test key |
| `87967f1` | `c6dfef8` | 2026-09-24 | fix: S4 setup runs sshd outside systemd and ensures host keys |
| `0faeb7d` | `edf9c2b` | 2026-09-24 | test: add S4 target setup and teardown scripts (verified Ubuntu WSL image) |
| `276e135` | `ab2e9b5` | 2026-09-24 | docs: record S2 Claude stream-json findings and fixtures |
| `c49fe9a` | `eafaa2c` | 2026-09-24 | fix: S2 sanitizer covers JSON-in-JSON escaping; add resanitize mode |
| `6e63f7b` | `f38fbea` | 2026-09-24 | test: S2 scenario for stdin close followed by a natural turn end |
| `7d358e3` | `ee55735` | 2026-09-24 | test: add spike S2 harness for Claude stream-json protocol |
| `7d38b51` | `ee94997` | 2026-09-24 | docs: record S1d console control results |
| `b42c776` | `51a07bf` | 2026-09-24 | test: add spike S1d for amirid console control and child Ctrl+C |
| `5346604` | `3d4ee36` | 2026-09-24 | docs: approve Ctrl+C handling, allowlist child env, S2 and S4 go-ahead |
| `75bf229` | `115e3ec` | 2026-09-24 | docs: record S1c WSL/tmux survival under a kill-on-close job |
| `5074fec` | `9eacfd8` | 2026-09-24 | test: add spike S1c for WSL processes under a kill-on-close job |
| `2b224fa` | `7a82e5e` | 2026-09-24 | docs: record S1 and S1b non-admin reruns |
| `0195d6b` | `d764f5f` | 2026-09-24 | test: S1 reads resize width as digits and can restore inherited Ctrl+C |
| `b35872f` | `feebe20` | 2026-09-24 | docs: record S0 environment baseline and EnableLUA doctor check |
| `3e4dbc7` | `181e475` | 2026-09-24 | docs: move ARCHITECTURE 9.6 after 9.5 |
| `49356ff` | `4ca46c3` | 2026-09-24 | docs: approve S5 reference-only and agent job-object containment |
| `3da9b00` | `d3d83f2` | 2026-09-24 | docs: record S5 static review of AgentsCommander |
| `e6614b3` | `853b594` | 2026-09-24 | docs: record S1b results and job-object recommendation |
| `8e98c8b` | `00cc8cf` | 2026-09-24 | test: add spike S1b for child lifetime when the host is killed |
| `b86fa90` | `bb7d0c8` | 2026-09-24 | docs: record S1 results from clean commit 62daad6 |
| `3684aeb` | `62daad6` | 2026-09-24 | chore: initial commit of architecture docs, env probe and spike S1 |
