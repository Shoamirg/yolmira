//! Slice B: the PTY integration suite (phase1-plan §1 Slice B, ARCHITECTURE §15).
//! Assertions use only locale-neutral values: markers the shell computes, numbers,
//! exit codes; never localized tool output.

mod support;

use serde_json::json;
use std::time::Duration;
use support::shell::{not_run, Shell, PTY_TEST};
use support::*;

const OS_MARK: &str = "YOLMIRA_Windows_NT";

async fn cmd(env: &TestEnv) -> Shell {
    Shell::open(env, json!({ "kind": "cmd" }), None)
        .await
        .expect("cmd session")
}

#[test]
fn echo_round_trip_cmd() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = cmd(&env).await;
        s.send("echo YOLMIRA_%OS%\r").await;
        s.expect(OS_MARK).await;
    });
}

#[test]
fn echo_round_trip_powershell() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None)
            .await
            .expect("powershell session");
        s.send("Write-Output (\"YOLMIRA_\" + (6*7))\r").await;
        s.expect("YOLMIRA_42").await;
    });
}

/// The foreground colour set by the last SGR sequences before `marker`.
fn foreground_before(text: &str, marker: &str) -> Option<String> {
    let end = text.rfind(marker)?;
    let mut fg = None;
    let mut rest = &text[..end];
    while let Some(i) = rest.find("\x1b[") {
        let after = &rest[i + 2..];
        let Some(m) = after.find(|c: char| c.is_ascii_alphabetic()) else {
            break;
        };
        if after.as_bytes()[m] == b'm' {
            let params: Vec<&str> = after[..m].split(';').collect();
            let mut k = 0;
            while k < params.len() {
                match params[k] {
                    "" | "0" | "39" => fg = None,
                    p @ ("30" | "31" | "32" | "33" | "34" | "35" | "36" | "37" | "90" | "91"
                    | "92" | "93" | "94" | "95" | "96" | "97") => fg = Some(p.to_string()),
                    "38" if params.get(k + 1) == Some(&"5") => {
                        fg = params.get(k + 2).map(|n| format!("5;{n}"));
                        k += 2;
                    }
                    "38" if params.get(k + 1) == Some(&"2") => {
                        fg = Some(format!(
                            "2;{}",
                            params
                                .get(k + 2..k + 5)
                                .map(|p| p.join(";"))
                                .unwrap_or_default()
                        ));
                        k += 4;
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        rest = &after[m + 1..];
    }
    fg
}

#[test]
fn ansi_color_semantics() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None)
            .await
            .expect("powershell session");
        s.send("Write-Host (\"YOLMIRA_\" + \"RED\" + (6*7)) -ForegroundColor Red\r")
            .await;
        s.expect("YOLMIRA_RED42").await;
        // ConPTY re-encodes colours (S1: 38;5;9 for Red); assert the attribute, not bytes.
        let fg = foreground_before(&s.text, "YOLMIRA_RED42").expect("a foreground colour is set");
        let red = ["31", "91", "5;1", "5;9"].contains(&fg.as_str())
            || fg.strip_prefix("2;").is_some_and(|rgb| {
                let v: Vec<u32> = rgb.split(';').filter_map(|x| x.parse().ok()).collect();
                v.len() == 3 && v[0] >= 128 && v[1] < 100 && v[2] < 100
            });
        assert!(red, "foreground {fg:?} is not red");
    });
}

#[test]
fn sgr_parser_reads_common_forms() {
    assert_eq!(
        foreground_before("\x1b[38;5;9mX", "X").as_deref(),
        Some("5;9")
    );
    assert_eq!(foreground_before("\x1b[91mA\x1b[0mX", "X"), None);
    assert_eq!(foreground_before("\x1b[1;31mX", "X").as_deref(), Some("31"));
    assert_eq!(
        foreground_before("\x1b[38;2;255;0;0mX", "X").as_deref(),
        Some("2;255;0;0")
    );
}

#[test]
fn resize_reported_by_child() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None).await.expect("powershell session");
        s.send("Write-Output (\"YOLMIRA_READY\" + (6*7))\r").await;
        s.expect("YOLMIRA_READY42").await;
        s.resize(100, 30).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        s.send("Write-Output (\"YOLMIRA_W\" + [Console]::WindowWidth + \"_H\" + [Console]::WindowHeight)\r").await;
        s.expect("YOLMIRA_W100_H30").await;
    });
}

#[test]
fn ctrl_c_interrupts_ping() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = cmd(&env).await;
        s.send("ping -t 127.0.0.1\r").await;
        // ping's lines are localized, but each reply names the address: wait until it
        // is really running (under load it can start late), then interrupt it.
        // `-t` never ends on its own. The echoed command line holds one occurrence.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while s.text.matches("127.0.0.1").count() < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "ping never ran: {:?}",
                support::shell::tail(&s.text)
            );
            s.expect_within("127.0.0.1", Duration::from_secs(5)).await;
            s.pump_once().await;
        }
        s.send_bytes(b"\x03").await;
        s.send("echo YOLMIRA_%OS%\r").await;
        s.expect(OS_MARK).await;
    });
}

#[test]
fn exit_codes() {
    let env = TestEnv::new();
    env.start();
    let id = env.block_on(PTY_TEST, async {
        let mut s = cmd(&env).await;
        s.send("exit 7\r").await;
        assert_eq!(s.wait_exit().await, Some(7));
        s.id.clone()
    });
    let list = last_json(&env.yolmira(&["shell", "list"]));
    let row = list
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == id))
        .cloned()
        .expect("listed");
    assert_eq!(
        (row["status"].as_str(), row["exit_code"].as_i64()),
        (Some("EXITED"), Some(7)),
        "{row}"
    );
    let exited = env
        .events()
        .into_iter()
        .find(|e| e["kind"] == "session.exited" && e["session_id"] == id)
        .expect("event");
    assert_eq!(exited["payload"]["exit_code"], 7);
}

#[test]
fn dsr_answered_without_client() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        // Create without attaching: only the daemon can answer the cursor request.
        let mut ctl = env.client().await;
        let info: yolmira_ipc::protocol::SessionInfo = ctl
            .call(
                "session.create",
                json!({ "shell": { "kind": "powershell" }, "cols": 120, "rows": 30 }),
            )
            .await
            .expect("create");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut s = Shell::attach(&env, &info.id, ctl).await.expect("attach");
        s.wait_powershell_prompt().await;
        s.send("Write-Output (\"YOLMIRA_\" + (6*7))\r").await;
        s.expect("YOLMIRA_42").await;
    });
}

#[test]
fn wsl_session() {
    // Distros are registered under HKCU\...\Lxss (locale-neutral, unlike `wsl -l`).
    let lxss = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).matches("\\{").count())
        .unwrap_or(0);
    if lxss == 0 {
        not_run("wsl_session", "no WSL distribution is registered");
        return;
    }
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "wsl" }), None)
            .await
            .expect("wsl session");
        s.send("echo YOLMIRA_$((6*7))\r").await;
        s.expect_within("YOLMIRA_42", Duration::from_secs(60)).await;
    });
}

#[test]
fn pwsh_session() {
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        match Shell::open(&env, json!({ "kind": "pwsh" }), None).await {
            Err(e) if e.contains("not installed") => {
                not_run("pwsh_session", "PowerShell 7 is not installed (risk R7)");
            }
            Err(e) => panic!("pwsh session: {e}"),
            Ok(mut s) => {
                s.send("Write-Output (\"YOLMIRA_\" + (6*7))\r").await;
                s.expect("YOLMIRA_42").await;
            }
        }
    });
}

#[test]
fn oem_codepage_output_decoded() {
    // cmd on ru-RU runs with OEM code page 866; ConPTY must still deliver UTF-8.
    let env = TestEnv::new();
    env.start();
    env.block_on(PTY_TEST, async {
        let mut s = cmd(&env).await;
        s.send("echo Привет_%OS%\r").await;
        s.expect("Привет_Windows_NT").await;
    });
}

#[test]
fn non_ascii_cwd() {
    let env = TestEnv::new();
    env.start();
    let dir = env.home().join("Проект oʻzbek");
    std::fs::create_dir_all(&dir).unwrap();
    env.block_on(PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "cmd" }), dir.to_str())
            .await
            .expect("session");
        s.send("cd\r").await;
        s.expect("Проект oʻzbek").await;
    });
}

#[test]
fn soak_1h_output_flood() {
    let Ok(secs) = std::env::var("YOLMIRA_SOAK").map(|v| v.parse::<u64>().unwrap_or(3600)) else {
        not_run(
            "soak_1h_output_flood",
            "set YOLMIRA_SOAK=<seconds> (3600 for the phase exit)",
        );
        return;
    };
    let env = TestEnv::new();
    let pid = env.start()["pid"].as_u64().expect("pid");
    env.block_on(Duration::from_secs(secs) + PTY_TEST, async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None).await.expect("session");
        let flood = format!(
            "$e=(Get-Date).AddSeconds({secs}); $i=0; while((Get-Date) -lt $e){{ $i++; Write-Output ('x'*200 + $i) }}; Write-Output (\"YOLMIRA_DONE\" + (6*7))\r"
        );
        s.send(&flood).await;
        s.expect_within("YOLMIRA_DONE42", Duration::from_secs(secs + 120)).await;
        s.send("Write-Output (\"YOLMIRA_AFTER\" + (6*7))\r").await;
        s.expect("YOLMIRA_AFTER42").await;
        // No lost exit status after the flood.
        s.send("exit 7\r").await;
        assert_eq!(s.wait_exit().await, Some(7), "exit status after the soak");
    });
    // Scrollback is bounded: the daemon's memory stays far below the flood volume.
    let ws =
        std::process::Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {pid}).WorkingSet64"),
            ])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
    assert!(
        ws > 0 && ws < 300 * 1024 * 1024,
        "daemon working set {ws} bytes"
    );
}
