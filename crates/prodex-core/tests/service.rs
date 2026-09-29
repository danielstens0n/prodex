use prodex_core::{ipc, model::*, service, store::Store};
use std::path::{Path, PathBuf};
use tokio::time::{Duration, Instant, sleep, timeout};

async fn call(dir: &Path, request: Request) -> serde_json::Value {
    let response = ipc::request(dir, request).await.expect("IPC response");
    assert!(response.ok, "{:?}", response.error);
    response.data
}

async fn snapshot(dir: &Path) -> Snapshot {
    serde_json::from_value(call(dir, Request::Status).await).unwrap()
}

async fn start(dir: PathBuf) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let state = dir.clone();
    let handle = tokio::spawn(async move { service::serve(&state).await });
    let deadline = Instant::now() + Duration::from_secs(5);
    while ipc::request(&dir, Request::Status).await.is_err() {
        if handle.is_finished() {
            panic!("daemon exited during startup: {:?}", handle.await);
        }
        assert!(Instant::now() < deadline, "daemon startup timed out");
        sleep(Duration::from_millis(50)).await;
    }
    handle
}

async fn stop(dir: &Path, handle: tokio::task::JoinHandle<anyhow::Result<()>>) {
    call(dir, Request::Shutdown).await;
    timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn proposal(project: &Path, prompt: &str) -> TaskProposal {
    TaskProposal {
        brief: None,
        risk: RiskLevel::Medium,
        project: project.into(),
        objective_version: 1,
        prompt: prompt.into(),
        rationale: "Independent integration test task".into(),
        provider: Provider::Mock,
        mode: TaskMode::ReadOnly,
        dependencies: vec![],
        expected_files: vec![],
        completion_criteria: "Complete the mock workflow".into(),
    }
}

async fn submit(dir: &Path, project: &Path, prompt: &str) -> TaskRecord {
    serde_json::from_value(
        call(
            dir,
            Request::Submit {
                proposal: proposal(project, prompt),
                approved: true,
            },
        )
        .await,
    )
    .unwrap()
}

async fn wait_status(dir: &Path, id: &str, expected: TaskStatus) -> TaskRecord {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let task = snapshot(dir)
            .await
            .tasks
            .into_iter()
            .find(|t| t.id == id)
            .unwrap();
        if task.status == expected {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, got {:?}: {}",
            task.status,
            task.summary
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn configure(dir: &Path, project: &Path) {
    call(
        dir,
        Request::Project {
            path: project.into(),
            objective: "Validate service lifecycle".into(),
            enabled: true,
        },
    )
    .await;
    call(
        dir,
        Request::Configure {
            settings: Settings {
                preferred_provider: Provider::Mock,
                planner_provider: Provider::Mock,
                max_concurrent: 1,
                max_per_project: 1,
                ..Settings::default()
            },
        },
    )
    .await;
}

#[tokio::test]
async fn executes_persists_and_refuses_a_second_daemon() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    let second = service::serve(dir.path()).await.unwrap_err();
    assert!(second.to_string().contains("another Prodex daemon"));
    // Failed lock acquisition must leave the first daemon's socket intact.
    assert_eq!(snapshot(dir.path()).await.projects.len(), 1);
    let task = submit(dir.path(), project.path(), "Complete first mock task").await;
    wait_status(dir.path(), &task.id, TaskStatus::Running).await;
    let finished = wait_status(dir.path(), &task.id, TaskStatus::Succeeded).await;
    assert!(finished.started_at.is_some());
    assert!(finished.updated_at >= finished.started_at.unwrap());
    assert!(finished.session_id.as_deref().unwrap().starts_with("mock-"));
    let events: Vec<Event> =
        serde_json::from_value(call(dir.path(), Request::Events { after: 0 }).await).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.task_id.as_deref() == Some(&task.id) && e.kind == "finished")
    );
    stop(dir.path(), handle).await;
    let restarted = start(dir.path().into()).await;
    let state = snapshot(dir.path()).await;
    assert_eq!(state.tasks[0].status, TaskStatus::Succeeded);
    assert_eq!(state.tasks[0].session_id, finished.session_id);
    assert!(state.settings.paused);
    stop(dir.path(), restarted).await;
}

#[tokio::test]
async fn pause_prevents_launch_and_stop_all_interrupts_running_and_queued() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    call(dir.path(), Request::Pause).await;
    let first = submit(dir.path(), project.path(), "First stoppable task").await;
    sleep(Duration::from_millis(450)).await;
    assert_eq!(
        snapshot(dir.path()).await.tasks[0].status,
        TaskStatus::Queued
    );
    call(dir.path(), Request::Resume).await;
    wait_status(dir.path(), &first.id, TaskStatus::Running).await;
    let second = submit(dir.path(), project.path(), "Second queued task").await;
    call(dir.path(), Request::StopAll).await;
    wait_status(dir.path(), &first.id, TaskStatus::Interrupted).await;
    wait_status(dir.path(), &second.id, TaskStatus::Interrupted).await;
    assert!(snapshot(dir.path()).await.settings.paused);
    stop(dir.path(), handle).await;
}

#[tokio::test]
async fn stale_settings_cannot_resume_a_paused_service() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    let mut stale_settings = snapshot(dir.path()).await.settings;
    assert!(!stale_settings.paused);
    call(dir.path(), Request::Pause).await;
    stale_settings.max_concurrent = 2;
    call(
        dir.path(),
        Request::Configure {
            settings: stale_settings,
        },
    )
    .await;
    let state = snapshot(dir.path()).await;
    assert!(state.settings.paused);
    assert_eq!(state.settings.max_concurrent, 2);
    stop(dir.path(), handle).await;
}

#[tokio::test]
async fn restart_quarantines_running_rows_without_killing_or_relaunching() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let project_path = project.path().canonicalize().unwrap();
    {
        let store = Store::open(&dir.path().join("state.sqlite3")).unwrap();
        store
            .save_project(&Project {
                use_worktrees: true,
                path: project_path.clone(),
                objective: "Recover safely".into(),
                objective_version: 1,
                enabled: true,
            })
            .unwrap();
        store
            .save_task(&TaskRecord {
                attempts: Vec::new(),
                automatic: false,
                started_at: None,
                id: "interrupted-by-crash".into(),
                proposal: proposal(&project_path, "Previously active task"),
                status: TaskStatus::Running,
                review: ReviewStatus::NotRequired,
                session_id: Some("existing-session".into()),
                worktree: None,
                branch: None,
                base_commit: None,
                // The test's own PID proves recovery does not blindly signal persisted PIDs.
                pid: Some(std::process::id()),
                summary: String::new(),
                created_at: now(),
                updated_at: now(),
            })
            .unwrap();
    }
    let handle = start(dir.path().into()).await;
    let state = snapshot(dir.path()).await;
    assert!(state.settings.paused);
    assert_eq!(state.tasks[0].status, TaskStatus::RecoveryRequired);
    assert_eq!(
        state.tasks[0].session_id.as_deref(),
        Some("existing-session")
    );
    let response = ipc::request(
        dir.path(),
        Request::Stop {
            id: "interrupted-by-crash".into(),
        },
    )
    .await
    .unwrap();
    assert!(!response.ok);
    call(
        dir.path(),
        Request::ResolveRecovery {
            id: "interrupted-by-crash".into(),
        },
    )
    .await;
    let state = snapshot(dir.path()).await;
    assert_eq!(state.tasks[0].status, TaskStatus::Interrupted);
    assert!(state.tasks[0].pid.is_none());
    stop(dir.path(), handle).await;
}

async fn wait_event(dir: &Path, kind: &str) -> Event {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let events: Vec<Event> =
            serde_json::from_value(call(dir, Request::Events { after: 0 }).await).unwrap();
        if let Some(event) = events.iter().find(|event| event.kind == kind) {
            return event.clone();
        }
        assert!(Instant::now() < deadline, "missing {kind}: {events:?}");
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn planner_can_abstain_and_cooldown_blocks_immediate_research() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    call(
        dir.path(),
        Request::Plan {
            project: project.path().into(),
        },
    )
    .await;
    let concurrent = ipc::request(
        dir.path(),
        Request::Plan {
            project: project.path().into(),
        },
    )
    .await
    .unwrap();
    assert!(!concurrent.ok);
    let completed = wait_event(dir.path(), "planning_completed").await;
    assert!(completed.message.contains("Created 0 useful proposals"));
    assert!(snapshot(dir.path()).await.tasks.is_empty());
    let again = ipc::request(
        dir.path(),
        Request::Plan {
            project: project.path().into(),
        },
    )
    .await
    .unwrap();
    assert!(!again.ok);
    assert!(again.error.unwrap().contains("cooldown"));
    stop(dir.path(), handle).await;
}

#[tokio::test]
async fn pause_cancels_ongoing_planning_without_creating_tasks() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    call(
        dir.path(),
        Request::Plan {
            project: project.path().into(),
        },
    )
    .await;
    wait_event(dir.path(), "planning_started").await;
    call(dir.path(), Request::Pause).await;
    wait_event(dir.path(), "planning_stopped").await;
    let state = snapshot(dir.path()).await;
    assert!(state.settings.paused);
    assert!(state.tasks.is_empty());
    let events: Vec<Event> =
        serde_json::from_value(call(dir.path(), Request::Events { after: 0 }).await).unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "planning_completed")
    );
    stop(dir.path(), handle).await;
}

#[tokio::test]
async fn allowed_folder_and_global_on_remain_idle_without_independent_codex() {
    let dir = tempfile::Builder::new()
        .prefix("pdx-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = tempfile::Builder::new()
        .prefix("pdx-p-")
        .tempdir_in("/tmp")
        .unwrap();
    let handle = start(dir.path().into()).await;
    configure(dir.path(), project.path()).await;
    call(
        dir.path(),
        Request::SetActivation {
            enabled: true,
            projects: Some(vec![project.path().into()]),
        },
    )
    .await;
    // Includes an observation tick and many scheduler ticks; free capacity alone
    // must never invoke even the mock planner.
    sleep(Duration::from_secs(6)).await;
    let state = snapshot(dir.path()).await;
    assert!(state.settings.planning_enabled && state.projects[0].enabled);
    assert!(state.observation.sessions.is_empty());
    assert!(state.tasks.is_empty());
    let events: Vec<Event> =
        serde_json::from_value(call(dir.path(), Request::Events { after: 0 }).await).unwrap();
    assert!(!events.iter().any(|event| event.kind == "planning_started"));
    let invalid = ipc::request(
        dir.path(),
        Request::SetActivation {
            enabled: true,
            projects: None,
        },
    )
    .await
    .unwrap();
    assert!(!invalid.ok);
    stop(dir.path(), handle).await;
}
