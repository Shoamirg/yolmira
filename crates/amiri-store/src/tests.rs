use super::*;
use amiri_core::event::{actors, kinds};
use serde_json::json;

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amiri.db");
    (dir, path)
}

fn session_events(store: &mut Store) {
    let created = |id: &str, kind: &str| {
        NewEvent::new(
            kinds::SESSION_CREATED,
            actors::USER,
            json!({ "shell_kind": kind, "cwd": "C:\\Проект" }),
        )
        .for_session(id)
    };
    store.append(created("s1", "cmd")).unwrap();
    store.append(created("s2", "powershell")).unwrap();
    store
        .append(
            NewEvent::new(
                kinds::SESSION_EXITED,
                actors::AMIRI,
                json!({ "exit_code": 7 }),
            )
            .for_session("s1"),
        )
        .unwrap();
    store
        .append(NewEvent::new(kinds::SESSION_UNKNOWN, actors::AMIRI, json!({})).for_session("s2"))
        .unwrap();
}

#[test]
fn fresh_database_migrates_to_latest_without_backup() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let report = store.migration_report();
    assert_eq!(
        (report.from_version, report.to_version),
        (0, LATEST_VERSION)
    );
    assert_eq!(report.backup, None);
    assert_eq!(store.integrity_check().unwrap(), "ok");
}

#[test]
fn migrations_apply_and_backup() {
    let (_dir, path) = temp_db();
    // A database from an older release: only migration 1, with one event in it.
    {
        let conn = Connection::open(&path).unwrap();
        migrate::apply_up_to(&conn, 1);
        conn.execute(
            "INSERT INTO events (id, ts, kind, actor, payload) VALUES ('e1', 't', 'daemon.started', 'amiri', '{}')",
            [],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let report = store.migration_report().clone();
    assert_eq!(
        (report.from_version, report.to_version),
        (1, LATEST_VERSION)
    );
    let backup = report
        .backup
        .expect("a backup is taken before migrating an existing database");
    // The backup is the pre-migration database: version 1, the event, no sessions table.
    let old = Connection::open(&backup).unwrap();
    let version: i64 = old
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let events: i64 = old
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let has_sessions: i64 = old
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sessions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((version, events, has_sessions), (1, 1, 0));
    // The migrated database kept the event.
    assert_eq!(store.list(0, 10, None).unwrap().len(), 1);
}

#[test]
fn events_are_ordered_and_filtered_by_kind_prefix() {
    let (_dir, path) = temp_db();
    let mut store = Store::open(&path).unwrap();
    store
        .append(NewEvent::new(
            kinds::DAEMON_STARTED,
            actors::AMIRI,
            json!({ "pid": 1 }),
        ))
        .unwrap();
    session_events(&mut store);
    let all = store.list(0, 100, None).unwrap();
    assert_eq!(all.len(), 5);
    assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(store.list(0, 100, Some("session.")).unwrap().len(), 4);
    // `_` and `%` are literal, not wildcards.
    assert_eq!(store.list(0, 100, Some("session_")).unwrap().len(), 0);
    assert_eq!(store.list(0, 100, Some("%")).unwrap().len(), 0);
    assert_eq!(store.list(all[2].seq, 100, None).unwrap().len(), 2);
    assert_eq!(store.last_seq().unwrap(), all[4].seq);
}

#[test]
fn events_persist_across_reopen() {
    let (_dir, path) = temp_db();
    let before = {
        let mut store = Store::open(&path).unwrap();
        session_events(&mut store);
        store.list(0, 100, None).unwrap()
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(store.list(0, 100, None).unwrap(), before);
}

#[test]
fn projection_rebuild_matches_live() {
    let (_dir, path) = temp_db();
    let mut store = Store::open(&path).unwrap();
    session_events(&mut store);
    let live = store.sessions().unwrap();
    assert_eq!(live.len(), 2);
    assert_eq!(
        store
            .session("s1")
            .unwrap()
            .map(|s| (s.status, s.exit_code)),
        Some(("EXITED".into(), Some(7)))
    );
    assert_eq!(
        store.session("s2").unwrap().map(|s| s.status),
        Some("UNKNOWN".into())
    );
    store.rebuild_projections().unwrap();
    assert_eq!(store.sessions().unwrap(), live);
}

#[test]
fn corrupt_payload_is_an_error_not_a_default() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute("INSERT INTO events (id, ts, kind, actor, payload) VALUES ('x', 't', 'k', 'a', 'not json')", [])
        .unwrap();
    assert!(matches!(
        store.list(0, 10, None),
        Err(StoreError::Payload(_))
    ));
}

fn project_event(kind: &'static str, payload: serde_json::Value) -> NewEvent {
    NewEvent::new(kind, actors::USER, payload)
}

#[test]
fn projects_are_registered_replaced_and_removed() {
    let (_dir, path) = temp_db();
    let mut store = Store::open(&path).unwrap();
    store
        .append(project_event(
            kinds::PROJECT_REGISTERED,
            json!({ "name": "web", "path": r"C:\src\web" }),
        ))
        .unwrap();
    store
        .append(project_event(
            kinds::PROJECT_REGISTERED,
            json!({ "name": "api", "path": r"C:\src\Проект" }),
        ))
        .unwrap();
    // Registering a name again moves it.
    store
        .append(project_event(
            kinds::PROJECT_REGISTERED,
            json!({ "name": "web", "path": r"D:\web" }),
        ))
        .unwrap();
    store
        .append(project_event(
            kinds::PROJECT_REMOVED,
            json!({ "name": "api" }),
        ))
        .unwrap();
    let projects = store.projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        (projects[0].name.as_str(), projects[0].path.as_str()),
        ("web", r"D:\web")
    );
    assert_eq!(store.project("web").unwrap().unwrap().path, r"D:\web");
    assert!(store.project("api").unwrap().is_none());
}

#[test]
fn sessions_remember_their_project_and_survive_a_rebuild() {
    let (_dir, path) = temp_db();
    let mut store = Store::open(&path).unwrap();
    store
        .append(project_event(
            kinds::PROJECT_REGISTERED,
            json!({ "name": "web", "path": r"C:\src\web" }),
        ))
        .unwrap();
    store
        .append(
            NewEvent::new(
                kinds::SESSION_CREATED,
                actors::USER,
                json!({ "shell_kind": "cmd", "cwd": r"C:\src\web", "project": "web" }),
            )
            .for_session("s1"),
        )
        .unwrap();
    session_events(&mut store);
    store.rebuild_projections().unwrap();
    assert_eq!(
        store.session("s1").unwrap().unwrap().project,
        None,
        "s1 was re-created without a project"
    );
    assert_eq!(store.projects().unwrap().len(), 1);
}

#[test]
fn a_session_created_in_a_project_keeps_it() {
    let (_dir, path) = temp_db();
    let mut store = Store::open(&path).unwrap();
    store
        .append(
            NewEvent::new(
                kinds::SESSION_CREATED,
                actors::USER,
                json!({ "shell_kind": "cmd", "cwd": r"C:\src\web", "project": "web" }),
            )
            .for_session("p1"),
        )
        .unwrap();
    store.rebuild_projections().unwrap();
    assert_eq!(
        store.session("p1").unwrap().unwrap().project.as_deref(),
        Some("web")
    );
}
