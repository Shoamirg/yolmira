//! Output budgets (Phase 1 exit checks): a client that reads slower than a shell writes
//! once made the daemon hold ~190 MB of queued scrollback copies. Now each connection's
//! queued output is capped, all connections together are capped, a slow client's queued
//! output is dropped and replaced by one fresh snapshot, and other clients don't notice.
//! Budgets are read back with the debug-only `debug.output_stats`.

mod support;

use serde_json::{json, Value};
use std::time::Duration;
use support::shell::Shell;
use support::*;

/// How long each shell floods. Long enough to overrun an 8 MiB budget at the slow rate.
const FLOOD_SECS: u64 = 15;
/// The slow client reads one notification (≤ 16 KiB of output) per this interval.
const SLOW_READ: Duration = Duration::from_millis(20);
const LIMIT: Duration = Duration::from_secs(600);

/// The test plays a UI window watching many shells: as the verified UI it may hold more
/// connections than an ordinary process (`daemon::slots`).
fn ui_env() -> TestEnv {
    let env = TestEnv::new();
    env.trust_this_test_as_ui();
    env.start();
    env
}

fn flood(marker: &str) -> String {
    format!(
        "$e=(Get-Date).AddSeconds({FLOOD_SECS}); $i=0; while((Get-Date) -lt $e){{ $i++; Write-Output ('L' + $i + 'E') }}; Write-Output (\"{marker}\" + (6*7))\r"
    )
}

async fn powershell(env: &TestEnv) -> Shell {
    let mut s = Shell::open(env, json!({ "kind": "powershell" }), None)
        .await
        .expect("powershell session");
    s.wait_powershell_prompt().await;
    s
}

/// Read as a slow client would, for `d`. Reads are never cancelled part-way (a line read
/// is not cancel-safe): each `pump_once` completes, then the client idles.
async fn read_slowly(s: &mut Shell, d: Duration) {
    let until = tokio::time::Instant::now() + d;
    while tokio::time::Instant::now() < until {
        s.pump_once().await;
        tokio::time::sleep(SLOW_READ).await;
    }
}

async fn stats(env: &TestEnv) -> Value {
    let mut c = env.client().await;
    c.call::<Value>("debug.output_stats", json!({}))
        .await
        .expect("debug.output_stats")
}

fn mb(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(f64::NAN) / 1048576.0
}

fn report(what: &str, v: &Value) {
    println!(
        "{what}: connection high water {:.1} of {:.1} MiB, daemon high water {:.1} of {:.1} MiB, resyncs {}",
        mb(v, "connection_high_water"),
        mb(v, "connection_limit"),
        mb(v, "daemon_high_water"),
        mb(v, "daemon_limit"),
        v["resyncs"]
    );
}

fn within_budgets(v: &Value) {
    let n = |k: &str| v[k].as_u64().unwrap_or(u64::MAX);
    assert!(
        n("connection_high_water") <= n("connection_limit"),
        "a connection went over its budget: {v}"
    );
    assert!(
        n("daemon_high_water") <= n("daemon_limit"),
        "the daemon went over its budget: {v}"
    );
    assert_eq!(
        n("daemon_used"),
        n("daemon_used").min(n("daemon_limit")),
        "{v}"
    );
}

#[test]
fn slow_client_stays_within_its_budget() {
    let env = ui_env();
    env.block_on(LIMIT, async {
        let mut s = powershell(&env).await;
        s.send(&flood("YOLMIRA_DONE")).await;
        read_slowly(&mut s, Duration::from_secs(FLOOD_SECS)).await;
        s.expect_within("YOLMIRA_DONE42", Duration::from_secs(120))
            .await;
        let v = stats(&env).await;
        report("one slow client", &v);
        within_budgets(&v);
        assert!(
            v["resyncs"].as_u64().unwrap_or(0) >= 1 && s.resets >= 2,
            "the slow client never fell behind, so the test proved nothing: {v}, resets {}",
            s.resets
        );
    });
}

#[test]
fn many_slow_clients_stay_within_the_daemon_budget() {
    // 3 flooding shells, 4 slow clients each: 12 x 8 MiB of demand against 64 MiB. A 13th,
    // fast client on the first shell must never be resynced, budgets full or not.
    let env = ui_env();
    env.block_on(LIMIT, async {
        let mut clients = Vec::new();
        for _ in 0..3 {
            let first = powershell(&env).await;
            let id = first.id.clone();
            clients.push(first);
            for _ in 0..3 {
                let ctl = env.client().await;
                clients.push(Shell::attach(&env, &id, ctl).await.expect("attach"));
            }
        }
        let ctl = env.client().await;
        let mut fast = Shell::attach(&env, &clients[0].id, ctl)
            .await
            .expect("attach");
        for i in [0, 4, 8] {
            clients[i].send(&flood("YOLMIRA_DONE")).await;
        }
        let fast_reader = tokio::spawn(async move {
            fast.expect_within("YOLMIRA_DONE42", Duration::from_secs(180))
                .await;
            fast
        });
        let mut readers = tokio::task::JoinSet::new();
        for mut c in clients {
            readers.spawn(async move {
                read_slowly(&mut c, Duration::from_secs(FLOOD_SECS)).await;
                c.expect_within("YOLMIRA_DONE42", Duration::from_secs(180))
                    .await;
            });
        }
        while let Some(r) = readers.join_next().await {
            r.expect("a slow client's reader");
        }
        let fast = fast_reader.await.expect("the fast client's reader");
        let v = stats(&env).await;
        report("12 slow clients and a fast one", &v);
        within_budgets(&v);
        assert!(
            v["resyncs"].as_u64().unwrap_or(0) >= 1,
            "the slow clients fell behind: {v}"
        );
        assert_eq!(
            fast.resets, 1,
            "the fast client was resynced: only its attach replay may reset it"
        );
    });
}

/// One shell, a fast client and a slow client attached; the shell floods, then exits.
async fn fast_and_slow(env: &TestEnv) -> (Shell, Shell) {
    let mut fast = powershell(env).await;
    let ctl = env.client().await;
    let mut slow = Shell::attach(env, &fast.id, ctl).await.expect("attach");
    fast.send(&flood("YOLMIRA_DONE")).await;
    // Concurrently: the fast client reads everything as it comes, the slow one idles
    // between reads.
    tokio::join!(
        fast.expect_within("YOLMIRA_DONE42", Duration::from_secs(120)),
        read_slowly(&mut slow, Duration::from_secs(FLOOD_SECS)),
    );
    fast.send("exit\r").await;
    assert_eq!(fast.wait_exit().await, Some(0));
    assert_eq!(slow.wait_exit().await, Some(0));
    (fast, slow)
}

#[test]
fn fast_client_on_the_same_shell_is_unaffected() {
    let env = ui_env();
    env.block_on(LIMIT, async {
        let (fast, slow) = fast_and_slow(&env).await;
        let v = stats(&env).await;
        report("fast and slow on one shell", &v);
        assert!(
            slow.resets >= 2,
            "the slow client never resynced: {} resets",
            slow.resets
        );
        assert_eq!(
            fast.resets, 1,
            "the fast client was resynced: only its attach replay may reset it"
        );
    });
}

#[test]
fn resynced_client_output_is_correct_and_gap_free() {
    let env = ui_env();
    env.block_on(LIMIT, async {
        let (fast, slow) = fast_and_slow(&env).await;
        assert!(slow.resets >= 2, "the slow client never resynced: {} resets", slow.resets);
        assert_eq!(fast.resets, 1, "the fast client's text is the reference: it must be whole");
        // Both texts end where the shell ended. The slow one starts at its last snapshot;
        // the fast one is the whole stream (trimmed to its latest part). The shorter must
        // be exactly the end of the longer: same bytes, nothing missing, nothing repeated.
        let (shorter, longer) = if slow.text.len() <= fast.text.len() {
            (&slow.text, &fast.text)
        } else {
            (&fast.text, &slow.text)
        };
        assert!(
            shorter.len() > 256 * 1024,
            "too little output to compare: {} bytes",
            shorter.len()
        );
        assert!(
            longer.ends_with(shorter.as_str()),
            "the resynced client's output differs from the stream: slow {} bytes, fast {} bytes\n--- slow tail ---\n{}\n--- fast tail ---\n{}",
            slow.text.len(),
            fast.text.len(),
            support::shell::tail(&slow.text),
            support::shell::tail(&fast.text)
        );
        assert!(slow.text.contains("YOLMIRA_DONE42"), "the slow client saw the end");
    });
}
