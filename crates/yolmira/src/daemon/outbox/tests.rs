use super::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

fn line(n: usize) -> String {
    "x".repeat(n - 1) // plus the newline `frame` adds: n bytes
}

fn outbox(global: &Arc<OutputBudget>, output: usize, control: usize) -> Arc<Outbox> {
    Outbox::with_limits(Arc::clone(global), output, control, STALL_LIMIT, 0)
}

#[test]
fn output_over_the_connection_budget_is_refused_and_counted_exactly() {
    let global = OutputBudget::new(1000);
    let out = outbox(&global, 100, 100);
    let s: Arc<str> = "s".into();
    assert_eq!(out.offer_output(&s, vec![line(60)]), Offer::Queued);
    assert_eq!(
        out.offer_output(&s, vec![line(41)]),
        Offer::Full,
        "60 + 41 > 100"
    );
    assert_eq!(
        out.offer_output(&s, vec![line(40)]),
        Offer::Queued,
        "exactly 100"
    );
    assert_eq!(global.stats().1, 100);
    assert_eq!(global.stats().3, 100, "per-connection high water");
}

#[test]
fn output_is_all_or_nothing_and_the_daemon_budget_binds_across_connections() {
    let global = OutputBudget::new(150);
    let a = outbox(&global, 100, 100);
    let b = outbox(&global, 100, 100);
    let s: Arc<str> = "s".into();
    assert_eq!(a.offer_output(&s, vec![line(50), line(50)]), Offer::Queued);
    assert_eq!(
        b.offer_output(&s, vec![line(30), line(30)]),
        Offer::Full,
        "100 + 60 > 150"
    );
    assert_eq!(b.offer_output(&s, vec![line(50)]), Offer::Queued);
    assert_eq!(global.stats().1, 150);
    drop(a);
    assert_eq!(global.stats().1, 50, "a dropped outbox returns its bytes");
}

#[test]
fn the_line_being_written_counts_until_the_writer_asks_for_the_next() {
    rt().block_on(async {
        let global = OutputBudget::new(1000);
        let out = outbox(&global, 100, 100);
        let s: Arc<str> = "s".into();
        out.offer_output(&s, vec![line(60)]);
        assert_eq!(out.next().await.map(|l| l.len()), Some(60));
        assert_eq!(global.stats().1, 60, "in flight: still counted");
        assert_eq!(
            out.offer_output(&s, vec![line(41)]),
            Offer::Full,
            "and still in the connection's budget"
        );
        out.finish();
        assert_eq!(out.next().await, None);
        assert_eq!(global.stats().1, 0, "written: released");
    });
}

#[test]
fn drop_output_removes_only_that_sessions_lines_and_keeps_order() {
    rt().block_on(async {
        let global = OutputBudget::new(10_000);
        let out = outbox(&global, 10_000, 10_000);
        let (s1, s2): (Arc<str>, Arc<str>) = ("s1".into(), "s2".into());
        out.offer_output(&s1, vec!["a".into()]);
        assert!(out.send("ctl".into()).await);
        out.offer_output(&s2, vec!["b".into()]);
        out.offer_output(&s1, vec!["c".into()]);
        out.drop_output(&s1);
        assert_eq!(global.stats().1, 2, "only s2's line is still counted");
        out.finish();
        let mut got = vec![];
        while let Some(l) = out.next().await {
            got.push(l);
        }
        assert_eq!(got, ["ctl\n", "b\n"]);
        assert_eq!(global.stats().1, 0);
    });
}

#[test]
fn control_lines_wait_for_room_and_are_never_dropped() {
    rt().block_on(async {
        let out = outbox(&OutputBudget::new(1000), 100, 10);
        assert!(out.send(line(8)).await);
        let waiting = {
            let out = Arc::clone(&out);
            tokio::spawn(async move { out.send(line(8)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "8 + 8 > 10: the sender waits");
        assert_eq!(out.next().await.map(|l| l.len()), Some(8));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "the first line is still being written"
        );
        // Asking for the next line marks the first written: room, so the second is queued.
        assert_eq!(out.next().await.map(|l| l.len()), Some(8));
        assert!(waiting.await.unwrap());
    });
}

#[test]
fn waiting_for_output_room_wakes_when_another_connection_frees_the_daemon_budget() {
    rt().block_on(async {
        let global = OutputBudget::new(100);
        let a = outbox(&global, 100, 100);
        let b = outbox(&global, 100, 100);
        let s: Arc<str> = "s".into();
        assert_eq!(a.offer_output(&s, vec![line(90)]), Offer::Queued);
        let waiter = {
            let b = Arc::clone(&b);
            tokio::spawn(async move { b.wait_output_room(50).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "the daemon budget is full");
        a.next().await;
        a.finish();
        a.next().await; // a's line written
        assert!(waiter.await.unwrap(), "a's write freed the daemon budget");
    });
}

#[test]
fn a_client_that_makes_no_room_for_the_stall_limit_is_closed_as_stalled() {
    rt().block_on(async {
        let global = OutputBudget::new(1000);
        let out = Outbox::with_limits(Arc::clone(&global), 100, 10, Duration::from_millis(100), 0);
        let s: Arc<str> = "s".into();
        out.offer_output(&s, vec![line(50)]);
        assert!(out.send(line(8)).await);
        let started = tokio::time::Instant::now();
        assert!(!out.send(line(8)).await, "no writer ever makes room");
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(out.stalled(), Some("responses and events over budget"));
        out.closed().await; // resolves: the connection ends
        assert_eq!(global.stats().1, 0, "its queued output is released");
        assert_eq!(out.offer_output(&s, vec![line(5)]), Offer::Closed);
    });
}

#[test]
fn a_closed_outbox_refuses_and_frees() {
    rt().block_on(async {
        let global = OutputBudget::new(1000);
        let out = outbox(&global, 100, 100);
        let s: Arc<str> = "s".into();
        out.offer_output(&s, vec![line(50)]);
        out.close();
        assert_eq!(global.stats().1, 0);
        assert_eq!(out.stalled(), None, "closed, not stalled");
        assert_eq!(out.offer_output(&s, vec![line(5)]), Offer::Closed);
        assert!(!out.send("x".into()).await);
        assert!(!out.wait_output_room(1).await);
        assert_eq!(out.next().await, None);
    });
}

#[test]
fn a_connection_below_its_floor_is_never_refused_by_the_full_daemon_budget() {
    let global = OutputBudget::new(100);
    let slow = Outbox::with_limits(Arc::clone(&global), 1000, 100, STALL_LIMIT, 50);
    let fast = Outbox::with_limits(Arc::clone(&global), 1000, 100, STALL_LIMIT, 50);
    let s: Arc<str> = "s".into();
    // The slow connection holds 150: its first 50 are its own, 100 fill the shared budget.
    assert_eq!(slow.offer_output(&s, vec![line(150)]), Offer::Queued);
    assert_eq!(global.stats().1, 100);
    assert_eq!(
        slow.offer_output(&s, vec![line(1)]),
        Offer::Full,
        "shared budget full"
    );
    // A connection that keeps its backlog under its floor still gets through.
    assert_eq!(fast.offer_output(&s, vec![line(40)]), Offer::Queued);
    assert_eq!(
        fast.offer_output(&s, vec![line(10)]),
        Offer::Queued,
        "exactly its floor"
    );
    assert_eq!(
        fast.offer_output(&s, vec![line(1)]),
        Offer::Full,
        "beyond it, it shares"
    );
    drop(slow);
    assert_eq!(
        global.stats().1,
        0,
        "the slow connection's shared bytes come back"
    );
}
