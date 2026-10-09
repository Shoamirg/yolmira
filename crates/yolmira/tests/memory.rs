//! Where `yolmirad`'s memory goes (Phase 1 exit checks, the soak's ~190 MB baseline).
//! A diagnostic, not a pass/fail gate: gated by YOLMIRA_MEMPROBE=<flood seconds>; it
//! prints the daemon's private bytes and working set at each stage. Run it on debug and
//! release builds (`cargo test --release`) to separate build overhead from the design.

mod support;

use serde_json::json;
use std::time::Duration;
use support::shell::{not_run, Shell, PTY_TEST};
use support::*;

/// (private MB, working set MB) of a process, via Windows PowerShell (locale-neutral numbers).
fn memory_mb(pid: u64) -> (f64, f64) {
    let out =
        std::process::Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$p = Get-Process -Id {pid}; \"$($p.PrivateMemorySize64) $($p.WorkingSet64)\""
                ),
            ])
            .output()
            .expect("powershell");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text
        .split_whitespace()
        .map(|n| n.parse::<f64>().unwrap_or(0.0) / 1048576.0);
    (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0))
}

fn stage(pid: u64, what: &str) {
    let (private, ws) = memory_mb(pid);
    println!("MEMPROBE {what:<46} private {private:>7.1} MB  working set {ws:>7.1} MB");
}

#[test]
fn memory_breakdown() {
    let Ok(secs) = std::env::var("YOLMIRA_MEMPROBE").map(|v| v.parse::<u64>().unwrap_or(60)) else {
        not_run("memory_breakdown", "set YOLMIRA_MEMPROBE=<flood seconds>");
        return;
    };
    let env = TestEnv::new();
    let pid = env.start()["pid"].as_u64().expect("pid");
    std::thread::sleep(Duration::from_secs(2));
    stage(pid, "daemon idle, no sessions");
    env.block_on(Duration::from_secs(secs) + PTY_TEST * 4, async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None).await.expect("session");
        s.wait_powershell_prompt().await;
        stage(pid, "1 PowerShell session, idle, client attached");
        let flood = format!(
            "$e=(Get-Date).AddSeconds({secs}); $i=0; while((Get-Date) -lt $e){{ $i++; Write-Output ('x'*200 + $i) }}; Write-Output (\"YOLMIRA_DONE\" + (6*7))\r"
        );
        s.send(&flood).await;
        s.expect_within("YOLMIRA_DONE42", Duration::from_secs(secs + 120)).await;
        stage(pid, &format!("after a {secs} s flood, client attached"));
        s.drop_connections();
        tokio::time::sleep(Duration::from_secs(3)).await;
        stage(pid, "after the flood, client detached");
        let mut more = Vec::new();
        for _ in 0..7 {
            let mut s = Shell::open(&env, json!({ "kind": "cmd" }), None).await.expect("session");
            s.send("echo YOLMIRA_%OS%\r").await;
            s.expect("YOLMIRA_Windows_NT").await;
            more.push(s);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        stage(pid, "8 sessions (1 flooded + 7 idle cmd)");
    });
}
