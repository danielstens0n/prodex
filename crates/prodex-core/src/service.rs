use crate::{
    ipc,
    model::*,
    observation, planner, policy,
    providers::RunSpec,
    runner::{self, WorkerEvent},
    store::Store,
    workspace,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::UnixListener,
    sync::{mpsc, oneshot},
};

const GIT_SETUP_PROMPT: &str = "Set up Git for this project, directly in the current project folder. This is the only authorized task here. Keep inspection output bounded: exclude target, node_modules, dist, build, vendor, caches and .git from recursive listings, use counts and small samples for large file sets, and summarize included files instead of dumping entire inventories. First inspect Git state; if this project already has a valid HEAD commit, stop without changing anything. Never initialize a nested repository inside an existing parent repository. If there is no repository, run git init in this folder. Review or add appropriate .gitignore entries for generated output, caches, dependencies, credentials and local environment files. Preserve all existing source files and user changes. Review the intended initial snapshot, stage explicit project source paths (not secrets, generated artifacts, unrelated folders, or existing unreviewed staged changes), and create an initial local commit. Use the configured Git author identity; if missing, report that the user needs to configure it rather than inventing an identity or changing global configuration. Do not delete files, rewrite history, add remotes, push, publish, or deploy. Verify git rev-parse --verify HEAD^{commit} and report the commit and included files. Do not implement any other coding tasks.";

enum Message {
    MergePrepared(
        String,
        std::result::Result<crate::integration::Baseline, String>,
    ),
    MergeVerified(String, std::result::Result<String, String>),
    Control(Request, oneshot::Sender<Response>),
    Worker(String, WorkerEvent),
    Planner(String, WorkerEvent),
    Prepared(String, std::result::Result<workspace::Workspace, String>),
    Observed(Observation),
    RepositoryChecked(
        PathBuf,
        std::result::Result<workspace::RepositoryState, String>,
    ),
    SetupPrepared(
        String,
        std::result::Result<workspace::RepositoryState, String>,
    ),
    SetupVerified(
        String,
        String,
        std::result::Result<workspace::RepositoryState, String>,
    ),
}

struct Planning {
    automatic: bool,
    project: Project,
    worker_provider: Provider,
    cancel: Option<oneshot::Sender<()>>,
    cancelled: bool,
}

struct Actor {
    store: Store,
    state_dir: PathBuf,
    workers: HashMap<String, oneshot::Sender<()>>,
    sender: mpsc::Sender<Message>,
    shutting_down: bool,
    planning: HashMap<String, Planning>,
    preparing: HashMap<String, bool>,
    observation: Observation,
    observing: bool,
    repositories: HashMap<PathBuf, (u64, std::result::Result<workspace::RepositoryState, String>)>,
    checking_repositories: std::collections::HashSet<PathBuf>,
}

/// File lock is held for the lifetime of the daemon; only its owner may replace the socket.
struct ServiceLock {
    _file: File,
    socket: PathBuf,
}
impl Drop for ServiceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub async fn serve(state_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let state_dir = state_dir.canonicalize()?;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(state_dir.join("daemon.lock"))?;
    // SAFETY: a valid owned descriptor; flock does not outlive it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another Prodex daemon owns this state directory");
    }
    let socket = state_dir.join("control.sock");
    let _lock = ServiceLock {
        _file: file,
        socket: socket.clone(),
    };
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)
        .context("bind local socket (use a shorter state path if it exceeds the OS limit)")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let store = Store::open(&state_dir.join("state.sqlite3"))?;
    store.recover()?;
    store.event(None, "service_started", "Prodex background service started")?;
    let (sender, mut receiver) = mpsc::channel(256);
    let mut actor = Actor {
        store,
        state_dir,
        workers: HashMap::new(),
        sender: sender.clone(),
        shutting_down: false,
        planning: HashMap::new(),
        preparing: HashMap::new(),
        observation: Observation::default(),
        observing: false,
        repositories: HashMap::new(),
        checking_repositories: Default::default(),
    };
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let mut observation_tick = tokio::time::interval(Duration::from_secs(5));
    observation_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let outcome: Result<()> = async {
    loop {
        tokio::select! {
            accepted = listener.accept(), if !actor.shutting_down => {
                let (stream, _) = accepted?;
                let sender = sender.clone();
                tokio::spawn(async move {
                    let response = tokio::time::timeout(Duration::from_secs(30), async {
                        let (read, mut write) = stream.into_split();
                        let mut reader = tokio::io::BufReader::new(read);
                        let response = match ipc::read_frame(&mut reader).await {
                            Ok(Some(bytes)) => match serde_json::from_slice::<Request>(&bytes) {
                                Ok(request) => {
                                    let (reply, receive) = oneshot::channel();
                                    if sender.send(Message::Control(request, reply)).await.is_err() { return Ok::<(), anyhow::Error>(()); }
                                    receive.await.unwrap_or_else(|_| Response::failure("service stopped"))
                                },
                                Err(error) => Response::failure(format!("invalid request: {error}")),
                            },
                            Ok(None) => return Ok(()),
                            Err(error) => Response::failure(error),
                        };
                        let mut bytes = serde_json::to_vec(&response)?;
                        if bytes.len() > ipc::MAX_FRAME - 1 {
                            bytes = serde_json::to_vec(&Response::failure("response too large; narrow or archive history"))?;
                        }
                        bytes.push(b'\n');
                        write.write_all(&bytes).await?;
                        Ok(())
                    }).await;
                    if let Ok(Err(error)) = response { eprintln!("IPC connection: {error:#}"); }
                });
            },
            Some(message) = receiver.recv() => match message {
                Message::Control(request, reply) => {
                    let response = match actor.control(request) {
                        Ok(value) => Response::success(value),
                        Err(error) => Response::failure(format!("{error:#}")),
                    };
                    let _ = reply.send(response);
                },
                Message::MergePrepared(id,result) => actor.merge_prepared(&id,result)?,
                Message::MergeVerified(id,result) => actor.merge_verified(&id,result)?,
                Message::Worker(id, event) => actor.handle_worker_event(&id, event)?,
                Message::Planner(id, event) => actor.planner_event(&id, event)?,
                Message::Prepared(id, result) => actor.prepared(&id, result)?,
                Message::Observed(observation) => actor.observed(observation)?,
                Message::RepositoryChecked(path,result)=>actor.repository_checked(path,result)?,
                Message::SetupPrepared(id,result)=>actor.setup_prepared(&id,result)?,
                Message::SetupVerified(id,summary,result)=>actor.setup_verified(&id,summary,result)?,
            },
            _ = tick.tick(), if !actor.shutting_down => { actor.schedule()?; actor.schedule_planning()?; },
            _ = observation_tick.tick(), if !actor.shutting_down && !actor.observing => actor.observe()?,
            _ = tokio::signal::ctrl_c(), if !actor.shutting_down => actor.begin_shutdown()?,
            _ = terminate.recv(), if !actor.shutting_down => actor.begin_shutdown()?,
        }
        if actor.shutting_down && actor.workers.is_empty() && actor.planning.is_empty() { break; }
    }
    actor.store.event(None, "service_stopped", "All managed workers stopped; worktrees preserved")?;
    Ok(())
    }.await;
    if outcome.is_err() {
        actor.cancel_live();
        actor.shutting_down = true;
        // Drain without relying on a healthy database. The runner owns and reaps
        // only its live process group; never signal persisted PIDs here.
        let cleanup = async {
            while !actor.workers.is_empty() || !actor.planning.is_empty() {
                match receiver.recv().await {
                    Some(Message::Worker(id, WorkerEvent::Finished { .. })) => {
                        actor.workers.remove(&id);
                    }
                    Some(Message::Planner(id, WorkerEvent::Finished { .. })) => {
                        actor.planning.remove(&id);
                    }
                    Some(Message::MergePrepared(id, result)) => {
                        let _ = actor.merge_prepared(&id, result);
                    }
                    Some(Message::MergeVerified(id, result)) => {
                        let _ = actor.merge_verified(&id, result);
                    }
                    Some(Message::Prepared(id, result)) => {
                        let _ = actor.prepared(&id, result);
                    }
                    Some(Message::SetupPrepared(id, result)) => {
                        let _ = actor.setup_prepared(&id, result);
                    }
                    Some(Message::SetupVerified(id, summary, result)) => {
                        let _ = actor.setup_verified(&id, summary, result);
                    }
                    Some(Message::Control(_, reply)) => {
                        let _ =
                            reply.send(Response::failure("service failed and is shutting down"));
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(70), cleanup)
            .await
            .is_err()
        {
            eprintln!(
                "Prodex cleanup timed out; preserved state requires recovery before retrying"
            );
        }
    }
    outcome
}

impl Actor {
    fn snapshot(&self) -> Result<Snapshot> {
        let mut snapshot = self.store.snapshot()?;
        snapshot.observation = self.observation.clone();
        if !snapshot
            .observation
            .checked_at
            .is_some_and(|time| time <= now() && now().saturating_sub(time) <= OBSERVATION_TTL_SECS)
        {
            snapshot.observation.available = false;
            snapshot.observation.detail =
                "Codex observation is not current; automatic work is waiting".into();
            snapshot.observation.sessions.clear();
        }
        snapshot.observation.sessions.retain(|s| {
            snapshot
                .projects
                .iter()
                .any(|p| p.enabled && p.path == s.project)
        });
        for project in &snapshot.projects {
            if policy::automatic_eligible(&project.path, &snapshot).is_err() {
                continue;
            }
            let next = self.store.next_plan_at(&project.path, &snapshot.settings)?;
            let daily_limit_reached =
                self.store.planning_passes_today()? >= snapshot.settings.max_plans_per_day;
            let (message, next_check_at) = if self
                .planning
                .values()
                .any(|p| p.project.path == project.path)
            {
                ("Looking for work…".into(), None)
            } else if let Some(setup) = snapshot.tasks.iter().find(|t| {
                t.proposal.project == project.path
                    && t.proposal.mode == TaskMode::InitializeRepository
                    && !matches!(t.status, TaskStatus::Succeeded | TaskStatus::Interrupted)
            }) {
                (
                    match setup.status {
                        TaskStatus::AwaitingApproval => "Git setup ready for approval",
                        TaskStatus::Rejected => {
                            "Git setup was rejected; coding needs an initial commit"
                        }
                        TaskStatus::NeedsRetry | TaskStatus::Failed => "Git setup needs attention",
                        _ => "Setting up Git…",
                    }
                    .into(),
                    None,
                )
            } else if let Some((_, Err(reason))) = self.repositories.get(&project.path) {
                (format!("Could not inspect Git: {reason}"), None)
            } else if self.checking_repositories.contains(&project.path) {
                ("Checking Git setup…".into(), None)
            } else if snapshot.tasks.iter().any(|t| {
                t.proposal.project == project.path
                    && matches!(t.status, TaskStatus::NeedsRetry | TaskStatus::Failed)
            }) {
                ("Task needs attention".into(), None)
            } else if snapshot.tasks.iter().any(|t| {
                t.proposal.project == project.path && t.status == TaskStatus::AwaitingApproval
            }) && planner::pending_count(project, &snapshot) >= planner::VISIBLE_PROPOSALS
            {
                ("Suggestions ready for approval".into(), None)
            } else if snapshot
                .tasks
                .iter()
                .any(|t| t.proposal.project == project.path && t.status == TaskStatus::Queued)
            {
                ("Work queued".into(), None)
            } else if daily_limit_reached {
                ("Daily planning limit reached".into(), next)
            } else if next.is_some_and(|time| time > now()) {
                (
                    self.store
                        .plan_result(&project.path)?
                        .unwrap_or_else(|| "Waiting to check again".into()),
                    next,
                )
            } else if !self.planning.is_empty() {
                ("Waiting for another project's check".into(), None)
            } else {
                let active = snapshot
                    .tasks
                    .iter()
                    .filter(|t| t.status.occupies_slot())
                    .count();
                let project_active = snapshot
                    .tasks
                    .iter()
                    .filter(|t| t.status.occupies_slot() && t.proposal.project == project.path)
                    .count();
                let threshold = snapshot
                    .settings
                    .planning_free_slots
                    .min(snapshot.settings.max_concurrent)
                    .min(snapshot.settings.max_per_project);
                let message = if self.store.starts_today()? >= snapshot.settings.max_starts_per_day
                {
                    "Daily task limit reached"
                } else if planner::pending_count(project, &snapshot) >= planner::VISIBLE_PROPOSALS
                    && (snapshot.settings.max_concurrent.saturating_sub(active) < threshold
                        || snapshot
                            .settings
                            .max_per_project
                            .saturating_sub(project_active)
                            < threshold)
                {
                    "Waiting for a free slot"
                } else {
                    "Ready to look for work"
                };
                (message.into(), None)
            };
            snapshot.planning_activity.push(PlanningActivity {
                project: project.path.clone(),
                message,
                next_check_at,
                daily_limit_reached,
            });
        }
        Ok(snapshot)
    }

    fn observe(&mut self) -> Result<()> {
        let projects = self.store.projects()?;
        let tasks = self.store.tasks()?;
        let mut excluded_ids = self.store.managed_sessions()?;
        excluded_ids.extend(tasks.iter().filter_map(|t| t.session_id.clone()));
        let mut excluded_roots: Vec<_> = tasks.iter().filter_map(|t| t.worktree.clone()).collect();
        excluded_roots.push(self.state_dir.join("worktrees"));
        self.observing = true;
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                observation::scan(projects, excluded_ids, excluded_roots),
            )
            .await;
            let observation = result.unwrap_or_else(|_| Observation {
                checked_at: Some(now()),
                available: false,
                detail: "Codex observation timed out; automatic work is waiting".into(),
                sessions: vec![],
            });
            let _ = sender.send(Message::Observed(observation)).await;
        });
        Ok(())
    }

    fn observed(&mut self, observation: Observation) -> Result<()> {
        self.observing = false;
        let previous: std::collections::BTreeSet<_> = self
            .observation
            .sessions
            .iter()
            .map(|s| s.project.clone())
            .collect();
        self.observation = observation;
        let snapshot = self.snapshot()?;
        let current: std::collections::BTreeSet<_> = snapshot
            .observation
            .sessions
            .iter()
            .map(|s| s.project.clone())
            .collect();
        for project in previous.symmetric_difference(&current) {
            self.store.event(
                None,
                if current.contains(project) {
                    "codex_observed"
                } else {
                    "codex_not_observed"
                },
                &format!(
                    "{}: {}",
                    project.display(),
                    if current.contains(project) {
                        "Independent Codex session observed"
                    } else {
                        "New automatic work waits for a Codex session"
                    }
                ),
            )?;
        }
        // Losing observation cancels research, never an already running worker.
        for plan in self.planning.values_mut() {
            if plan.automatic && policy::automatic_eligible(&plan.project.path, &snapshot).is_err()
            {
                plan.cancelled = true;
                if let Some(cancel) = plan.cancel.take() {
                    let _ = cancel.send(());
                }
            }
        }
        Ok(())
    }

    fn control(&mut self, request: Request) -> Result<serde_json::Value> {
        if self.shutting_down {
            bail!("service is shutting down");
        }
        match request {
            Request::SetActivation { enabled, projects } => {
                self.set_activation(enabled, projects)?
            }
            Request::Status => return Ok(serde_json::to_value(self.snapshot()?)?),
            Request::Events { after } => {
                return Ok(serde_json::to_value(self.store.events(after)?)?);
            }
            Request::SetWorktrees { project, enabled } => {
                let mut project = self.store.project(&project)?;
                let tasks = self.store.tasks()?;
                if tasks
                    .iter()
                    .any(|t| t.proposal.project == project.path && t.status.occupies_slot())
                {
                    bail!(
                        "Stop or finish this project's active task before changing its workspace setting"
                    );
                }
                project.use_worktrees = enabled;
                self.store.save_project(&project)?;
                if !enabled {
                    let setup_ids: Vec<_> = tasks
                        .iter()
                        .filter(|t| {
                            t.proposal.project == project.path
                                && t.proposal.mode == TaskMode::InitializeRepository
                        })
                        .map(|t| t.id.clone())
                        .collect();
                    for mut task in tasks {
                        if task.proposal.project != project.path {
                            continue;
                        }
                        if task.proposal.mode == TaskMode::InitializeRepository
                            && matches!(
                                task.status,
                                TaskStatus::AwaitingApproval
                                    | TaskStatus::Queued
                                    | TaskStatus::NeedsRetry
                            )
                        {
                            task.status = TaskStatus::Interrupted;
                            task.summary =
                                "Git setup is not required while main-folder editing is enabled"
                                    .into();
                            self.store.save_task(&task)?;
                        } else if matches!(
                            task.status,
                            TaskStatus::AwaitingApproval
                                | TaskStatus::Queued
                                | TaskStatus::NeedsRetry
                                | TaskStatus::Failed
                        ) {
                            task.proposal
                                .dependencies
                                .retain(|id| !setup_ids.contains(id));
                            self.store.save_task(&task)?;
                        }
                    }
                }
                self.store.event(
                    None,
                    "workspace_mode_changed",
                    &format!(
                        "{}: {}",
                        project.path.display(),
                        if enabled {
                            "worktrees"
                        } else {
                            "main folder; one coding task at a time"
                        }
                    ),
                )?;
            }
            Request::Merge { id } => return self.request_merge(&id),
            Request::ConfirmIntegrated { id } => {
                let mut task = self.store.task(&id)?;
                if task.status != TaskStatus::Succeeded
                    || task.worktree.is_none()
                    || task.proposal.mode != TaskMode::Edit
                    || self.workers.contains_key(&id)
                {
                    bail!("Only completed worktree tasks can be confirmed as integrated");
                }
                task.review = ReviewStatus::Integrated;
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store.event(Some(&id), "integration_acknowledged", "User confirmed the changes have been reviewed and integrated outside Prodex; no Git operation was performed")?;
            }
            Request::MarkReviewed { id } => {
                let mut task = self.store.task(&id)?;
                if task.status != TaskStatus::Succeeded
                    || task.proposal.mode != TaskMode::EditInPlace
                    || self.workers.contains_key(&id)
                {
                    bail!("Only completed main-folder changes can be marked reviewed");
                }
                task.review = ReviewStatus::Integrated;
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store.event(
                    Some(&id),
                    "reviewed",
                    "User reviewed changes already in the main project folder",
                )?;
            }
            Request::Configure { mut settings } => {
                // Pause/resume are lifecycle commands; stale GUI forms must not undo them.
                let current = self.store.settings()?;
                settings.paused = current.paused;
                settings.project_scope = current.project_scope;
                self.store.save_settings(&settings)?;
                self.store
                    .event(None, "settings_changed", "Service settings updated")?;
            }
            Request::Project {
                path,
                objective,
                enabled,
            } => {
                let path = path
                    .canonicalize()
                    .context("project directory does not exist")?;
                if !path.is_dir() {
                    bail!("project must be a directory");
                }
                if objective.trim().is_empty() || objective.len() > 16_384 {
                    bail!("objective must contain 1..16384 bytes");
                }
                let old = self.store.project(&path).ok();
                let objective_version = old.as_ref().map_or(1, |p| {
                    p.objective_version + u64::from(p.objective != objective)
                });
                let mut settings = self.store.settings()?;
                let scope = settings.project_scope.get_or_insert_default();
                scope.retain(|entry| entry != &path);
                if enabled {
                    scope.push(path.clone());
                    scope.sort();
                }
                self.store.save_activation(
                    &settings,
                    &[Project {
                        use_worktrees: old.as_ref().is_none_or(|p| p.use_worktrees),
                        path: path.clone(),
                        objective,
                        objective_version,
                        enabled,
                    }],
                )?;
                // Revocation invalidates running research before it can publish.
                for plan in self
                    .planning
                    .values_mut()
                    .filter(|p| p.project.path == path)
                {
                    plan.cancelled = true;
                    if let Some(cancel) = plan.cancel.take() {
                        let _ = cancel.send(());
                    }
                }
                self.store.event(
                    None,
                    "project_changed",
                    "Project objective or enabled state updated",
                )?;
            }
            Request::Submit { proposal, approved } => {
                if matches!(
                    proposal.mode,
                    TaskMode::InitializeRepository | TaskMode::EditInPlace | TaskMode::Merge
                ) {
                    bail!(
                        "Execution modes are coordinator-owned; submit an edit task and choose the project workspace in Settings"
                    );
                }
                return self.submit(proposal, approved, false);
            }
            Request::Retry { id } => {
                let mut task = self.store.task(&id)?;
                if !matches!(task.status, TaskStatus::NeedsRetry | TaskStatus::Failed)
                    || self.workers.contains_key(&id)
                    || self.preparing.contains_key(&id)
                {
                    bail!("only stopped tasks needing a retry can be retried");
                }
                let mut candidate = task.clone();
                candidate.status = TaskStatus::Queued;
                let mut snapshot = self.snapshot()?;
                snapshot.tasks.retain(|t| t.id != id);
                policy::eligible(&candidate, &snapshot, self.store.starts_today()?)
                    .map_err(anyhow::Error::msg)?;
                task.attempts.push(TaskAttempt {
                    status: task.status,
                    session_id: task.session_id.take(),
                    worktree: task.worktree.take(),
                    branch: task.branch.take(),
                    base_commit: task.base_commit.take(),
                    summary: std::mem::take(&mut task.summary),
                    started_at: task.started_at.take(),
                    finished_at: task.updated_at,
                });
                if task.proposal.mode == TaskMode::Merge {
                    let source = self.store.task(
                        task.proposal
                            .dependencies
                            .first()
                            .context("Merge source missing")?,
                    )?;
                    task.worktree = source.worktree;
                    task.branch = source.branch;
                    task.base_commit = source.base_commit;
                }
                task.status = TaskStatus::Queued;
                task.review = ReviewStatus::NotRequired;
                task.pid = None;
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store.event(
                    Some(&id),
                    "retry_requested",
                    "User requested a fresh attempt; previous worktrees and sessions preserved",
                )?;
            }
            Request::Approve { id } => {
                let mut task = self.store.task(&id)?;
                policy::coding_task(&task.proposal).map_err(anyhow::Error::msg)?;
                if task.status != TaskStatus::AwaitingApproval {
                    bail!("task is not awaiting approval");
                }
                let project = self.store.project(&task.proposal.project)?;
                if project.objective_version != task.proposal.objective_version {
                    bail!("proposal is stale; submit a new proposal against the current objective");
                }
                task.status = TaskStatus::Queued;
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store
                    .event(Some(&id), "approved", "User approved this task")?;
            }
            Request::Reject { id } => {
                let mut task = self.store.task(&id)?;
                let dismiss_result = task.status == TaskStatus::Succeeded
                    && matches!(
                        task.review,
                        ReviewStatus::AwaitingReview | ReviewStatus::Accepted
                    )
                    && matches!(task.proposal.mode, TaskMode::Edit | TaskMode::EditInPlace);
                if !dismiss_result
                    && !matches!(
                        task.status,
                        TaskStatus::AwaitingApproval
                            | TaskStatus::Queued
                            | TaskStatus::NeedsRetry
                            | TaskStatus::Failed
                    )
                {
                    bail!("only pending tasks or unintegrated results can be dismissed");
                }
                if dismiss_result
                    && self.store.tasks()?.iter().any(|job| {
                        job.proposal.mode == TaskMode::Merge
                            && job.proposal.dependencies == [id.clone()]
                            && (job.status.occupies_slot() || job.status == TaskStatus::Queued)
                    })
                {
                    bail!("Stop the active merge before dismissing this result");
                }
                task.status = TaskStatus::Rejected;
                if dismiss_result {
                    task.review = ReviewStatus::Rejected;
                }
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store.event(
                    Some(&id),
                    "rejected",
                    if dismiss_result {
                        "Result dismissed; files, worktree and sessions preserved. Not integrated."
                    } else {
                        "Task rejected"
                    },
                )?;
            }
            Request::Pause => self.pause(true)?,
            Request::Resume => self.pause(false)?,
            Request::Stop { id } => self.stop(&id)?,
            Request::StopAll => self.stop_all()?,
            Request::UnconnectProject { project } => {
                self.unconnect_project(&project)?;
            }
            Request::CheckNow { project } => {
                let project = self.store.project(&project.canonicalize()?)?;
                let id = self.start_planning(project, true, true)?;
                return Ok(serde_json::json!({"planning_id": id}));
            }
            Request::Plan { project } => {
                let project = self.store.project(&project.canonicalize()?)?;
                let id = self.start_planning(project, false, false)?;
                return Ok(serde_json::json!({"planning_id": id}));
            }
            Request::ResolveRecovery { id } => {
                let mut task = self.store.task(&id)?;
                if task.status != TaskStatus::RecoveryRequired {
                    bail!("task does not require recovery");
                }
                task.status = TaskStatus::Interrupted;
                task.pid = None;
                task.updated_at = now();
                self.store.save_task(&task)?;
                self.store.event(
                    Some(&id),
                    "recovery_resolved",
                    "User confirmed old worker is stopped. No retry scheduled.",
                )?;
            }
            Request::Shutdown => self.begin_shutdown()?,
        }
        Ok(serde_json::json!({"accepted":true}))
    }

    fn request_merge(&mut self, id: &str) -> Result<serde_json::Value> {
        let source = self.store.task(id)?;
        if source.status != TaskStatus::Succeeded
            || source.proposal.mode != TaskMode::Edit
            || source.worktree.is_none()
        {
            bail!("Only completed coding worktrees can be merged");
        }
        if source.review == ReviewStatus::Integrated {
            bail!("Task is already integrated");
        }
        let snapshot = self.snapshot()?;
        if snapshot.settings.paused {
            bail!("Resume Prodex before starting a merge");
        }
        if !self.store.project(&source.proposal.project)?.enabled {
            bail!("Project is disabled");
        }
        if let Some(job) = snapshot
            .tasks
            .iter()
            .rev()
            .find(|t| t.proposal.mode == TaskMode::Merge && t.proposal.dependencies == [id])
        {
            if job.status.occupies_slot() || job.status == TaskStatus::Queued {
                bail!("Integration is already active or awaiting recovery");
            }
            if matches!(job.status, TaskStatus::NeedsRetry | TaskStatus::Failed) {
                return self.control(Request::Retry { id: job.id.clone() });
            }
        }
        let mut job = source.clone();
        job.id = uuid::Uuid::new_v4().to_string();
        job.automatic = false;
        job.attempts.clear();
        job.started_at = None;
        job.pid = None;
        job.session_id = None;
        job.status = TaskStatus::Queued;
        job.review = ReviewStatus::NotRequired;
        job.created_at = now();
        job.updated_at = now();
        job.summary.clear();
        job.proposal.mode = TaskMode::Merge;
        job.proposal.dependencies = vec![id.into()];
        job.proposal.expected_files.clear();
        job.proposal.objective_version = self
            .store
            .project(&source.proposal.project)?
            .objective_version;
        job.proposal.brief = None;
        job.proposal.prompt = format!("Merge completed task {id} locally");
        job.proposal.rationale = "User requested background local integration".into();
        self.store.save_task(&job)?;
        self.store.event(
            Some(&job.id),
            "merge_requested",
            &format!("Background integration of {id}; completion requires Git verification"),
        )?;
        Ok(serde_json::to_value(job)?)
    }

    fn merge_prepared(
        &mut self,
        id: &str,
        result: std::result::Result<crate::integration::Baseline, String>,
    ) -> Result<()> {
        let cancelled = self.preparing.remove(id).unwrap_or(true);
        let mut job = self.store.task(id)?;
        let result = if cancelled || self.shutting_down || job.status != TaskStatus::Starting {
            Err("Merge cancelled before launch".into())
        } else {
            result
        };
        match result {
            Ok(baseline) => {
                self.store.save_merge_baseline(id, &baseline)?;
                if baseline.already_integrated {
                    let source = self.store.task(&job.proposal.dependencies[0])?;
                    return self.merge_verified(
                        id,
                        crate::integration::verify(&source, &baseline)
                            .map_err(|e| format!("{e:#}")),
                    );
                }
                let mut snapshot = self.snapshot()?;
                snapshot.tasks.retain(|t| t.id != id);
                job.status = TaskStatus::Queued;
                if let Err(error) = policy::eligible(
                    &job,
                    &snapshot,
                    self.store.starts_today()?.saturating_sub(1),
                ) {
                    return self.worker_event(
                        id,
                        WorkerEvent::Finished {
                            success: false,
                            interrupted: true,
                            summary: error,
                        },
                    );
                }
                self.launch_worker(&job, snapshot.settings.task_timeout_secs)
            }
            Err(error) => self.worker_event(
                id,
                WorkerEvent::Finished {
                    success: false,
                    interrupted: cancelled || self.shutting_down,
                    summary: error,
                },
            ),
        }
    }

    fn merge_verified(
        &mut self,
        id: &str,
        result: std::result::Result<String, String>,
    ) -> Result<()> {
        let mut job = self.store.task(id)?;
        if self.shutting_down || job.status == TaskStatus::Stopping {
            return self.worker_event(
                id,
                WorkerEvent::Finished {
                    success: false,
                    interrupted: true,
                    summary: "Integration stopped; verify Git state before retrying".into(),
                },
            );
        }
        match result {
            Ok(summary) => {
                let mut source = self.store.task(
                    job.proposal
                        .dependencies
                        .first()
                        .context("Merge source missing")?,
                )?;
                self.store.complete_merge(&mut job, &mut source, summary)?;
                self.workers.remove(id);
                self.store
                    .event(Some(&source.id), "integration_verified", &job.summary)
            }
            Err(error) => self.worker_event(
                id,
                WorkerEvent::Finished {
                    success: false,
                    interrupted: false,
                    summary: if job.summary.trim().is_empty() {
                        format!("Merge could not be verified: {error}")
                    } else {
                        format!("{}\n\nGit verification: {error}", job.summary.trim())
                    },
                },
            ),
        }
    }

    fn submit(
        &mut self,
        mut proposal: TaskProposal,
        approved: bool,
        automatic: bool,
    ) -> Result<serde_json::Value> {
        proposal.project = proposal.project.canonicalize()?;
        policy::validate_proposal(&proposal, &self.store.snapshot()?)
            .map_err(anyhow::Error::msg)?;
        let task = TaskRecord {
            attempts: Vec::new(),
            automatic,
            started_at: None,
            id: uuid::Uuid::new_v4().to_string(),
            proposal,
            status: if approved {
                TaskStatus::Queued
            } else {
                TaskStatus::AwaitingApproval
            },
            review: ReviewStatus::NotRequired,
            session_id: None,
            worktree: None,
            branch: None,
            base_commit: None,
            pid: None,
            summary: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.save_task(&task)?;
        self.store
            .event(Some(&task.id), "submitted", &task.proposal.rationale)?;
        Ok(serde_json::to_value(task)?)
    }

    fn unconnect_project(&mut self, path: &Path) -> Result<()> {
        // Stored canonical identity also permits removing a folder that has moved.
        let mut project = self.store.project(path)?;
        let tasks: Vec<_> = self
            .store
            .tasks()?
            .into_iter()
            .filter(|t| t.proposal.project == path)
            .collect();
        if tasks
            .iter()
            .any(|t| t.status == TaskStatus::RecoveryRequired)
        {
            bail!("Resolve this project's worker recovery before unconnecting it");
        }
        let mut settings = self.store.settings()?;
        settings
            .project_scope
            .get_or_insert_default()
            .retain(|p| p != path);
        project.enabled = false;
        self.store.save_activation(&settings, &[project])?;
        for plan in self
            .planning
            .values_mut()
            .filter(|p| p.project.path == path)
        {
            plan.cancelled = true;
            if let Some(cancel) = plan.cancel.take() {
                let _ = cancel.send(());
            }
        }
        // Signal all owned work before status writes, even if a later write fails.
        for task in &tasks {
            if let Some(cancelled) = self.preparing.get_mut(&task.id) {
                *cancelled = true;
            }
            if let Some(cancel) = self.workers.get_mut(&task.id) {
                let (placeholder, _) = oneshot::channel();
                let _ = std::mem::replace(cancel, placeholder).send(());
            }
        }
        for task in tasks {
            if self.workers.contains_key(&task.id)
                || matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::AwaitingApproval
                )
            {
                self.stop(&task.id)?;
            }
        }
        self.store.event(
            None,
            "project_unconnected",
            "Project unconnected; owned work stopped, files and history preserved",
        )
    }

    fn set_activation(&mut self, enabled: bool, selection: Option<Vec<PathBuf>>) -> Result<()> {
        if selection.is_none() {
            bail!("select explicit project folders; all-project activation is no longer supported");
        }
        let mut projects = self.store.projects()?;
        let selection = selection
            .map(|paths| -> Result<Vec<PathBuf>> {
                let mut canonical = Vec::new();
                for path in paths {
                    let path = path
                        .canonicalize()
                        .context("selected project does not exist")?;
                    if !projects.iter().any(|project| project.path == path) {
                        bail!("selected project is not configured: {}", path.display());
                    }
                    if !canonical.contains(&path) {
                        canonical.push(path);
                    }
                }
                canonical.sort();
                Ok(canonical)
            })
            .transpose()?;
        // Complete validation and reads before any persistence or cancellation.
        let tasks = self.store.tasks()?;
        let mut settings = self.store.settings()?;
        // Updating selection or repeating On must not undo an explicit pause.
        // Off -> On reactivates; an already enabled paused service needs Resume.
        settings.paused = !enabled || (settings.planning_enabled && settings.paused);
        settings.planning_enabled = enabled;
        settings.project_scope = selection;
        for project in &mut projects {
            project.enabled = settings
                .project_scope
                .as_ref()
                .is_some_and(|scope| scope.contains(&project.path));
        }
        self.store.save_activation(&settings, &projects)?;
        // No scheduler can interleave this actor operation. Cancel every old
        // planner so none can publish proposals under the former selection.
        for plan in self.planning.values_mut() {
            plan.cancelled = true;
            if let Some(cancel) = plan.cancel.take() {
                let _ = cancel.send(());
            }
        }
        // Signal all affected live workers before status writes; a storage error
        // during one status update must not leave another excluded worker running.
        for task in &tasks {
            let selected = settings
                .project_scope
                .as_ref()
                .is_some_and(|scope| scope.contains(&task.proposal.project));
            if !enabled || !selected {
                if let Some(cancelled) = self.preparing.get_mut(&task.id) {
                    *cancelled = true;
                }
                if let Some(cancel) = self.workers.get_mut(&task.id) {
                    let (placeholder, _) = oneshot::channel();
                    let _ = std::mem::replace(cancel, placeholder).send(());
                }
            }
        }
        for task in tasks {
            let selected = settings
                .project_scope
                .as_ref()
                .is_some_and(|scope| scope.contains(&task.proposal.project));
            if (!enabled || !selected)
                && (self.workers.contains_key(&task.id)
                    || matches!(
                        task.status,
                        TaskStatus::Queued | TaskStatus::AwaitingApproval
                    ))
            {
                self.stop(&task.id)?;
            }
        }
        self.store.event(
            None,
            "activation_changed",
            if enabled {
                "Prodex enabled for the selected projects"
            } else {
                "Prodex disabled; managed work stopped and preserved"
            },
        )
    }

    fn pause(&mut self, paused: bool) -> Result<()> {
        let mut settings = self.store.settings()?;
        settings.paused = paused;
        self.store.save_settings(&settings)?;
        if paused {
            for cancelled in self.preparing.values_mut() {
                *cancelled = true;
            }
            for plan in self.planning.values_mut() {
                plan.cancelled = true;
                if let Some(cancel) = plan.cancel.take() {
                    let _ = cancel.send(());
                }
            }
        }
        self.store.event(
            None,
            if paused { "paused" } else { "resumed" },
            if paused {
                "New launches paused; existing workers continue"
            } else {
                "Scheduling resumed"
            },
        )
    }

    fn stop(&mut self, id: &str) -> Result<()> {
        let mut task = self.store.task(id)?;
        if let Some(cancelled) = self.preparing.get_mut(id) {
            *cancelled = true;
        }
        if let Some(cancel) = self.workers.remove(id) {
            // Retain an entry until Finished so shutdown cannot exit before the worker is reaped.
            let (placeholder, _) = oneshot::channel();
            self.workers.insert(id.into(), placeholder);
            let _ = cancel.send(());
            task.status = TaskStatus::Stopping;
        } else if matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::AwaitingApproval
        ) {
            task.status = TaskStatus::Interrupted;
        } else if task.status == TaskStatus::RecoveryRequired {
            bail!(
                "worker ownership is unknown after restart; verify it stopped and use resolve-recovery"
            );
        } else {
            bail!("task is not active");
        }
        task.updated_at = now();
        self.store.save_task(&task)?;
        self.store.event(
            Some(id),
            "stop_requested",
            "Stop requested; worktree and history will be preserved",
        )
    }

    fn cancel_live(&mut self) {
        for cancelled in self.preparing.values_mut() {
            *cancelled = true;
        }
        for cancel in self.workers.values_mut() {
            let (placeholder, _) = oneshot::channel();
            let _ = std::mem::replace(cancel, placeholder).send(());
        }
        for plan in self.planning.values_mut() {
            plan.cancelled = true;
            if let Some(cancel) = plan.cancel.take() {
                let _ = cancel.send(());
            }
        }
    }

    fn stop_all(&mut self) -> Result<()> {
        // Cancellation must work even when SQLite cannot persist pause/status.
        self.cancel_live();
        self.pause(true)?;
        for task in self.store.tasks()? {
            if self.workers.contains_key(&task.id)
                || matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::AwaitingApproval
                )
            {
                self.stop(&task.id)?;
            }
        }
        Ok(())
    }

    fn begin_shutdown(&mut self) -> Result<()> {
        self.shutting_down = true;
        self.stop_all()?;
        Ok(())
    }

    fn schedule(&mut self) -> Result<()> {
        for mut task in self.store.tasks()? {
            if task.status != TaskStatus::Queued {
                continue;
            }
            let project = self.store.project(&task.proposal.project)?;
            if matches!(task.proposal.mode, TaskMode::Edit | TaskMode::EditInPlace) {
                task.proposal.mode = if project.use_worktrees {
                    TaskMode::Edit
                } else {
                    TaskMode::EditInPlace
                };
            }
            if task.proposal.mode == TaskMode::Edit
                && task.proposal.provider != Provider::Mock
                && !matches!(self.repositories.get(&task.proposal.project), Some((checked, Ok(workspace::RepositoryState::Ready))) if now().saturating_sub(*checked) < 10)
            {
                if !self
                    .repositories
                    .get(&task.proposal.project)
                    .is_some_and(|(at, _)| now().saturating_sub(*at) < 10)
                {
                    self.check_repository(&self.store.project(&task.proposal.project)?);
                }
                continue;
            }
            let snapshot = self.snapshot()?;
            if policy::eligible(&task, &snapshot, self.store.starts_today()?).is_err() {
                continue;
            }
            self.store.reserve(&mut task)?;
            if task.proposal.mode == TaskMode::Merge {
                let source = self.store.task(
                    task.proposal
                        .dependencies
                        .first()
                        .context("Merge source missing")?,
                )?;
                let (placeholder, _) = oneshot::channel();
                self.workers.insert(task.id.clone(), placeholder);
                self.preparing.insert(task.id.clone(), false);
                let sender = self.sender.clone();
                let id = task.id.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        crate::integration::prepare(&source).map_err(|e| format!("{e:#}"))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = sender.send(Message::MergePrepared(id, result)).await;
                });
            } else if task.proposal.mode == TaskMode::InitializeRepository {
                let (placeholder, _) = oneshot::channel();
                self.workers.insert(task.id.clone(), placeholder);
                self.preparing.insert(task.id.clone(), false);
                let path = task.proposal.project.clone();
                let id = task.id.clone();
                let sender = self.sender.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        workspace::repository_state(&path).map_err(|e| format!("{e:#}"))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = sender.send(Message::SetupPrepared(id, result)).await;
                });
            } else if task.proposal.mode == TaskMode::Edit {
                // Preparation holds its reserved slot, but never blocks the actor.
                // Its placeholder keeps shutdown waiting until Prepared arrives.
                let (placeholder, _) = oneshot::channel();
                self.workers.insert(task.id.clone(), placeholder);
                self.preparing.insert(task.id.clone(), false);
                let project = task.proposal.project.clone();
                let root = self.state_dir.join("worktrees");
                let id = task.id.clone();
                let preparation_id = if task.attempts.is_empty() {
                    id.clone()
                } else {
                    format!("{id}-retry-{}", task.attempts.len())
                };
                let sender = self.sender.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        workspace::create_worktree(&project, &root, &preparation_id)
                            .map_err(|error| format!("{error:#}"))
                    })
                    .await
                    .unwrap_or_else(|error| Err(format!("workspace preparation failed: {error}")));
                    let _ = sender.send(Message::Prepared(id, result)).await;
                });
            } else {
                if task.proposal.mode == TaskMode::EditInPlace {
                    task.review = ReviewStatus::AwaitingReview;
                    self.store.save_task(&task)?;
                }
                self.launch_worker(&task, snapshot.settings.task_timeout_secs)?;
            }
        }
        Ok(())
    }

    fn prepared(
        &mut self,
        id: &str,
        result: std::result::Result<workspace::Workspace, String>,
    ) -> Result<()> {
        let cancelled = self.preparing.remove(id).unwrap_or(true);
        // Preparation is now done. A launched runner will replace this entry.
        self.workers.remove(id);
        let mut task = self.store.task(id)?;
        match result {
            Ok(workspace) => {
                task.worktree = Some(workspace.path);
                task.branch = Some(workspace.branch);
                task.base_commit = Some(workspace.base_commit);
                task.review = ReviewStatus::AwaitingReview;
                self.store.save_task(&task)?;
            }
            Err(error) => {
                task.status =
                    if cancelled || self.shutting_down || task.status == TaskStatus::Stopping {
                        TaskStatus::Interrupted
                    } else {
                        TaskStatus::NeedsRetry
                    };
                task.summary = format!(
                    "Workspace preparation failed: {error}; any partial workspace is preserved"
                );
                task.updated_at = now();
                self.store.save_task(&task)?;
                return self
                    .store
                    .event(Some(id), "preparation_failed", &task.summary);
            }
        }
        let mut snapshot = self.snapshot()?;
        let timeout = snapshot.settings.task_timeout_secs;
        // Reservation already counts against both capacity and daily starts.
        // Validate a queued view excluding our own reservation to avoid counting
        // it twice while still rechecking all current policy and dependencies.
        snapshot.tasks.retain(|other| other.id != task.id);
        let mut candidate = task.clone();
        candidate.status = TaskStatus::Queued;
        let starts_before_reservation = self.store.starts_today()?.saturating_sub(1);
        let blocked = if cancelled || self.shutting_down || task.status != TaskStatus::Starting {
            Some("preparation was cancelled before provider launch".to_owned())
        } else {
            policy::eligible(&candidate, &snapshot, starts_before_reservation).err()
        };
        if let Some(reason) = blocked {
            task.status = TaskStatus::Interrupted;
            task.summary =
                format!("Provider was not launched: {reason}; prepared worktree preserved");
            task.updated_at = now();
            self.store.save_task(&task)?;
            return self
                .store
                .event(Some(id), "preparation_cancelled", &task.summary);
        }
        self.launch_worker(&task, timeout)
    }

    fn launch_worker(&mut self, task: &TaskRecord, timeout_secs: u64) -> Result<()> {
        let scope = if task.proposal.expected_files.is_empty() {
            "whole project (unspecified file scope)".to_owned()
        } else {
            task.proposal.expected_files.join(", ")
        };
        let mut spec = RunSpec {
            integration_project: None,
            provider: task.proposal.provider,
            cwd: task
                .worktree
                .clone()
                .unwrap_or_else(|| task.proposal.project.clone()),
            prompt: format!(
                "{}\nProject objective: {}\n\nTask: {}\n\nCompletion criteria: {}\n\nExpected file scope (advisory): {}\n\nStay within this task and its expected file scope. Do not merge, push, publish, or deploy. Report changes and validation.\n",
                if task.proposal.mode == TaskMode::EditInPlace {
                    "You are editing the user's main project folder directly, alongside their own work. Inspect existing changes first. Preserve unrelated edits; do not reset, stash, clean, switch branches or commit. Only implement this task. Stop and report if its changes cannot safely coexist with existing edits."
                } else {
                    ""
                },
                self.store.project(&task.proposal.project)?.objective,
                if task.proposal.mode == TaskMode::InitializeRepository {
                    GIT_SETUP_PROMPT
                } else {
                    &task.proposal.prompt
                },
                task.proposal.completion_criteria,
                scope
            ),
            mode: task.proposal.mode,
            session_id: None,
        };
        if task.proposal.mode == TaskMode::Merge {
            let source = self.store.task(
                task.proposal
                    .dependencies
                    .first()
                    .context("Merge source missing")?,
            )?;
            spec.integration_project = Some(task.proposal.project.clone());
            spec.prompt =
                crate::integration::prompt(&source, &self.store.merge_baseline(&task.id)?);
        }
        let (cancel, receive_cancel) = oneshot::channel();
        self.workers.insert(task.id.clone(), cancel);
        let (events, mut receive_events) = mpsc::channel(64);
        let sender = self.sender.clone();
        let id = task.id.clone();
        tokio::spawn(async move {
            while let Some(event) = receive_events.recv().await {
                if sender
                    .send(Message::Worker(id.clone(), event))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        tokio::spawn(runner::run(spec, timeout_secs, receive_cancel, events));
        Ok(())
    }

    fn check_repository(&mut self, project: &Project) {
        if self.checking_repositories.contains(&project.path) {
            return;
        }
        self.checking_repositories.insert(project.path.clone());
        let path = project.path.clone();
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let check = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                workspace::repository_state(&check).map_err(|e| format!("{e:#}"))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            let _ = sender.send(Message::RepositoryChecked(path, result)).await;
        });
    }

    fn repository_checked(
        &mut self,
        path: PathBuf,
        result: std::result::Result<workspace::RepositoryState, String>,
    ) -> Result<()> {
        self.checking_repositories.remove(&path);
        self.repositories
            .insert(path.clone(), (now(), result.clone()));
        if self.shutting_down {
            return Ok(());
        }
        if let Ok(state) = result {
            if state != workspace::RepositoryState::Ready {
                self.ensure_git_setup(&path)?;
            } else {
                for mut task in self.store.tasks()? {
                    if task.proposal.project == path
                        && task.proposal.mode == TaskMode::InitializeRepository
                        && matches!(
                            task.status,
                            TaskStatus::AwaitingApproval
                                | TaskStatus::Queued
                                | TaskStatus::NeedsRetry
                                | TaskStatus::Failed
                                | TaskStatus::Rejected
                                | TaskStatus::Interrupted
                        )
                        && !self.workers.contains_key(&task.id)
                    {
                        task.status = TaskStatus::Succeeded;
                        task.summary =
                            "Git repository and initial commit verified; setup is complete.".into();
                        task.updated_at = now();
                        self.store.save_task(&task)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn ensure_git_setup(&mut self, path: &Path) -> Result<()> {
        let snapshot = self.snapshot()?;
        if policy::automatic_eligible(path, &snapshot).is_err() {
            return Ok(());
        }
        if snapshot
            .tasks
            .iter()
            .any(|t| t.proposal.project == path && t.status.occupies_slot())
        {
            return Ok(());
        }
        let project = self.store.project(path)?;
        if !project.use_worktrees {
            return Ok(());
        }
        let existing = snapshot.tasks.iter().find(|t| {
            t.proposal.project == path
                && t.proposal.objective_version == project.objective_version
                && t.proposal.mode == TaskMode::InitializeRepository
                && !matches!(t.status, TaskStatus::Interrupted | TaskStatus::Succeeded)
        });
        let id = if let Some(task) = existing {
            task.id.clone()
        } else {
            let task=TaskRecord {
                attempts:Vec::new(),automatic:true,started_at:None,id:uuid::Uuid::new_v4().to_string(),
                proposal:TaskProposal {
                    brief:Some(TaskBrief {title:"Create a Git repository and initial commit".into(),change:"Set up version control so Prodex can run coding tasks in isolated worktrees.".into(),approach:"Run a setup session directly in this project. Initialize Git if needed, review ignore rules and source files, then create the initial commit.".into()}),
                    project:path.into(),objective_version:project.objective_version,
                    prompt:GIT_SETUP_PROMPT.into(),rationale:"This project has no usable Git commit, which is required for coding worktrees.".into(),provider:snapshot.settings.preferred_provider,mode:TaskMode::InitializeRepository,
                    dependencies:Vec::new(),expected_files:Vec::new(),completion_criteria:"Git resolves this project to a working-tree repository with a valid HEAD commit. Existing project files are preserved and credentials/build artifacts are excluded from the commit.".into(),risk:RiskLevel::Medium,
                },
                status:TaskStatus::AwaitingApproval,review:ReviewStatus::NotRequired,session_id:None,worktree:None,branch:None,base_commit:None,pid:None,summary:String::new(),created_at:now(),updated_at:now(),
            };
            self.store.save_task(&task)?;
            self.store.event(
                Some(&task.id),
                "git_setup_suggested",
                "Git setup requires approval and runs directly in the project folder",
            )?;
            task.id
        };
        for mut task in snapshot.tasks {
            if task.proposal.project == path
                && matches!(task.proposal.mode, TaskMode::Edit | TaskMode::EditInPlace)
                && matches!(
                    task.status,
                    TaskStatus::AwaitingApproval
                        | TaskStatus::Queued
                        | TaskStatus::NeedsRetry
                        | TaskStatus::Failed
                )
                && !task.proposal.dependencies.contains(&id)
            {
                task.proposal.dependencies.push(id.clone());
                self.store.save_task(&task)?;
            }
        }
        Ok(())
    }

    fn setup_prepared(
        &mut self,
        id: &str,
        result: std::result::Result<workspace::RepositoryState, String>,
    ) -> Result<()> {
        let cancelled = self.preparing.remove(id).unwrap_or(true);
        self.workers.remove(id);
        let mut task = self.store.task(id)?;
        if cancelled || self.shutting_down || task.status == TaskStatus::Stopping {
            task.status = TaskStatus::Interrupted;
            task.summary = "Git setup was cancelled before launch".into();
        } else {
            match result {
                Ok(workspace::RepositoryState::Ready) => {
                    task.status = TaskStatus::Succeeded;
                    task.summary =
                        "Git setup is already complete; no direct editing session was launched."
                            .into();
                }
                Ok(_) => {
                    let mut snapshot = self.snapshot()?;
                    snapshot.tasks.retain(|t| t.id != id);
                    let mut candidate = task.clone();
                    candidate.status = TaskStatus::Queued;
                    if let Err(error) = policy::eligible(
                        &candidate,
                        &snapshot,
                        self.store.starts_today()?.saturating_sub(1),
                    ) {
                        task.status = TaskStatus::Interrupted;
                        task.summary = error;
                    } else {
                        return self.launch_worker(&task, snapshot.settings.task_timeout_secs);
                    }
                }
                Err(error) => {
                    task.status = TaskStatus::NeedsRetry;
                    task.summary = format!("Git setup could not start: {error}");
                }
            }
        }
        task.updated_at = now();
        self.store.save_task(&task)
    }

    fn handle_worker_event(&mut self, id: &str, event: WorkerEvent) -> Result<()> {
        if let WorkerEvent::Finished {
            success: true,
            interrupted: false,
            ref summary,
        } = event
        {
            let task = self.store.task(id)?;
            if task.proposal.mode == TaskMode::Merge {
                let source = self.store.task(
                    task.proposal
                        .dependencies
                        .first()
                        .context("Merge source missing")?,
                )?;
                let baseline = self.store.merge_baseline(id)?;
                // Keep the agent's actionable blocker if independent verification fails.
                let mut task = task;
                task.summary = summary.clone();
                self.store.save_task(&task)?;
                let sender = self.sender.clone();
                let id = id.to_owned();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        crate::integration::verify(&source, &baseline).map_err(|e| format!("{e:#}"))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = sender.send(Message::MergeVerified(id, result)).await;
                });
                return Ok(());
            }
            if task.proposal.mode == TaskMode::InitializeRepository {
                let path = task.proposal.project;
                let sender = self.sender.clone();
                let id = id.to_owned();
                let summary = summary.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        workspace::repository_state(&path).map_err(|e| format!("{e:#}"))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    let _ = sender
                        .send(Message::SetupVerified(id, summary, result))
                        .await;
                });
                return Ok(());
            }
        }
        self.worker_event(id, event)
    }

    fn setup_verified(
        &mut self,
        id: &str,
        summary: String,
        result: std::result::Result<workspace::RepositoryState, String>,
    ) -> Result<()> {
        let task = self.store.task(id)?;
        let interrupted = task.status == TaskStatus::Stopping || self.shutting_down;
        let success = matches!(result, Ok(workspace::RepositoryState::Ready)) && !interrupted;
        let summary = if success {
            summary
        } else {
            format!(
                "Git setup did not produce a usable initial commit. {}",
                result.err().unwrap_or(summary)
            )
        };
        self.repositories.remove(&task.proposal.project);
        self.worker_event(
            id,
            WorkerEvent::Finished {
                success,
                interrupted,
                summary,
            },
        )
    }

    fn schedule_planning(&mut self) -> Result<()> {
        let settings = self.store.settings()?;
        if settings.paused || !settings.planning_enabled || !self.planning.is_empty() {
            return Ok(());
        }
        for project in self.store.projects()? {
            if project.use_worktrees
                && settings.preferred_provider != Provider::Mock
                && policy::automatic_eligible(&project.path, &self.snapshot()?).is_ok()
            {
                let fresh = self
                    .repositories
                    .get(&project.path)
                    .is_some_and(|(at, _)| now().saturating_sub(*at) < 10);
                if !fresh {
                    self.check_repository(&project);
                    continue;
                }
                match self.repositories.get(&project.path).map(|(_, r)| r) {
                    Some(Ok(workspace::RepositoryState::Ready)) => {}
                    Some(Ok(_)) => {
                        self.ensure_git_setup(&project.path)?;
                        continue;
                    }
                    _ => continue,
                }
            }
            if self.planning_eligible(&project).is_ok()
                && policy::automatic_eligible(&project.path, &self.snapshot()?).is_ok()
            {
                self.start_planning(project, true, false)?;
                break; // One planner across the service to bound research costs.
            }
        }
        Ok(())
    }

    fn planning_eligible(&self, project: &Project) -> Result<()> {
        self.planning_eligible_with_cooldown(project, false)
    }

    fn planning_eligible_with_cooldown(
        &self,
        project: &Project,
        skip_cooldown: bool,
    ) -> Result<()> {
        let snapshot = self.snapshot()?;
        if snapshot.settings.paused || !project.enabled {
            bail!("planning is paused or project is disabled");
        }
        if !self.planning.is_empty() {
            bail!("a planning pass is already running");
        }
        let passes = self.store.planning_passes_today()?;
        if passes >= snapshot.settings.max_plans_per_day {
            bail!(
                "daily planning limit reached ({passes}/{} checks); resets at 00:00 UTC",
                snapshot.settings.max_plans_per_day
            );
        }
        let mut limits = snapshot.settings.clone();
        if skip_cooldown {
            limits.planner_cooldown_secs = 0;
        }
        if !self.store.plan_allowed(&project.path, &limits)? {
            bail!("planner cooldown active; use Check now to skip the wait");
        }
        if self.store.starts_today()? >= snapshot.settings.max_starts_per_day {
            bail!("daily worker-start limit reached");
        }
        let threshold = snapshot
            .settings
            .planning_free_slots
            .min(snapshot.settings.max_concurrent)
            .min(snapshot.settings.max_per_project);
        let global_free = snapshot.settings.max_concurrent.saturating_sub(
            snapshot
                .tasks
                .iter()
                .filter(|task| task.status.occupies_slot())
                .count(),
        );
        let project_free = snapshot.settings.max_per_project.saturating_sub(
            snapshot
                .tasks
                .iter()
                .filter(|task| task.proposal.project == project.path && task.status.occupies_slot())
                .count(),
        );
        let pending = planner::pending_count(project, &snapshot);
        if pending >= planner::MAX_PROPOSALS {
            bail!("suggestion backlog is full");
        }
        // Refill the visible choices even when all workers are busy. The
        // configured free-slot threshold still gates replenishing the reserve.
        if pending >= planner::VISIBLE_PROPOSALS
            && (global_free < threshold || project_free < threshold)
        {
            bail!("reserve planning requires at least {threshold} free worker slots");
        }
        Ok(())
    }

    fn start_planning(
        &mut self,
        project: Project,
        automatic: bool,
        skip_cooldown: bool,
    ) -> Result<String> {
        if automatic {
            policy::automatic_eligible(&project.path, &self.snapshot()?)
                .map_err(anyhow::Error::msg)?;
        }
        if project.use_worktrees
            && self.store.settings()?.preferred_provider != Provider::Mock
            && automatic
        {
            match self.repositories.get(&project.path).map(|(_, r)| r) {
                Some(Ok(workspace::RepositoryState::Ready)) => {}
                Some(Ok(_)) => {
                    self.ensure_git_setup(&project.path)?;
                    bail!("Approve the Git setup task before checking for coding work");
                }
                Some(Err(error)) => bail!("Cannot inspect project Git state: {error}"),
                None => {
                    self.check_repository(&project);
                    bail!("Checking project Git setup; try again shortly");
                }
            }
        }
        self.planning_eligible_with_cooldown(&project, skip_cooldown)?;
        let snapshot = self.snapshot()?;
        let notes = self.store.project_notes(&project.path)?;
        let spec = planner::spec(
            &project,
            &snapshot,
            snapshot.settings.planner_provider,
            notes.as_ref(),
        );
        let id = uuid::Uuid::new_v4().to_string();
        self.store.record_plan(&id, &project.path)?;
        self.store.event(
            None,
            "planning_started",
            &format!(
                "{}: researching useful independent work (pass {id})",
                project.path.display()
            ),
        )?;
        let (cancel, receive_cancel) = oneshot::channel();
        self.planning.insert(
            id.clone(),
            Planning {
                automatic,
                project,
                worker_provider: snapshot.settings.preferred_provider,
                cancel: Some(cancel),
                cancelled: false,
            },
        );
        let (events, mut receive_events) = mpsc::channel(64);
        let sender = self.sender.clone();
        let event_id = id.clone();
        tokio::spawn(async move {
            while let Some(event) = receive_events.recv().await {
                if sender
                    .send(Message::Planner(event_id.clone(), event))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        tokio::spawn(runner::run(
            spec,
            snapshot.settings.task_timeout_secs.min(300),
            receive_cancel,
            events,
        ));
        Ok(id)
    }

    fn planner_event(&mut self, id: &str, event: WorkerEvent) -> Result<()> {
        match event {
            WorkerEvent::Diagnostic(message) => {
                self.store
                    .event(None, "planner_debug", &format!("plan={id} {message}"))?;
            }
            WorkerEvent::Started { pid } => {
                self.store.event(
                    None,
                    "planner_debug",
                    &format!("plan={id} started pid={pid:?}"),
                )?;
            }
            WorkerEvent::Finished {
                success,
                interrupted,
                summary,
            } => {
                let Some(plan) = self.planning.remove(id) else {
                    return Ok(());
                };
                self.store.finish_plan(id)?;
                if !success || interrupted || plan.cancelled {
                    self.store.save_plan_result(
                        &plan.project.path,
                        if interrupted || plan.cancelled {
                            "Previous check stopped"
                        } else {
                            "Could not finish the last check"
                        },
                    )?;
                    self.store.event(None, "planning_stopped", &summary)?;
                    return Ok(());
                }
                let snapshot = self.snapshot()?;
                let current = self.store.project(&plan.project.path)?;
                if (plan.automatic
                    && policy::automatic_eligible(&plan.project.path, &snapshot).is_err())
                    || snapshot.settings.paused
                    || !current.enabled
                    || current.objective_version != plan.project.objective_version
                {
                    self.store.event(
                        None,
                        "planning_discarded",
                        "Project or pause state changed during research",
                    )?;
                    return Ok(());
                }
                let output =
                    match planner::parse_output(&summary, &plan.project, plan.worker_provider) {
                        Ok(proposals) => proposals,
                        Err(error) => {
                            self.store.save_plan_result(
                                &plan.project.path,
                                "Could not read the planner response",
                            )?;
                            self.store
                                .event(None, "planning_failed", &format!("{error:#}"))?;
                            return Ok(());
                        }
                    };
                if let Some(notes) = output.notes {
                    self.store.save_project_notes(&current, &notes)?;
                    self.store.event(
                        None,
                        "project_notes_updated",
                        &format!("Updated planner notebook for {}", current.path.display()),
                    )?;
                }
                let available = planner::MAX_PROPOSALS
                    .saturating_sub(planner::pending_count(&current, &snapshot));
                let mut accepted = 0;
                for proposal in output.proposals {
                    if proposal.risk > snapshot.settings.max_proposal_risk {
                        self.store.event(
                            None,
                            "proposal_discarded",
                            "Planner proposal exceeds the configured risk ceiling",
                        )?;
                        continue;
                    }
                    if accepted >= available {
                        break;
                    }
                    let approved = false;
                    match self.submit(proposal, approved, plan.automatic) {
                        Ok(_) => accepted += 1,
                        Err(error) => {
                            self.store
                                .event(None, "proposal_discarded", &error.to_string())?
                        }
                    }
                }
                self.store.event(
                    None,
                    "planning_completed",
                    &format!("Created {accepted} useful proposals; zero is a valid outcome"),
                )?;
                self.store.save_plan_result(
                    &plan.project.path,
                    if accepted == 0 {
                        "No new work found"
                    } else {
                        "Suggestions created"
                    },
                )?;
            }
            WorkerEvent::Output(crate::providers::ProviderOutput::Error(error)) => {
                self.store.event(None, "planner_error", &error)?
            }
            WorkerEvent::Output(crate::providers::ProviderOutput::Session(session)) => {
                self.store.record_managed_session(&session)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn worker_event(&mut self, id: &str, event: WorkerEvent) -> Result<()> {
        use crate::providers::ProviderOutput;
        if matches!(&event, WorkerEvent::Finished { .. }) {
            self.workers.remove(id);
        }
        let mut task = self.store.task(id)?;
        match event {
            WorkerEvent::Diagnostic(message) => {
                // Diagnostics must not change task ordering or rewrite task state.
                return self.store.event(Some(id), "worker_debug", &message);
            }
            WorkerEvent::Started { pid } => {
                task.started_at = Some(now());
                task.pid = pid;
                if task.status != TaskStatus::Stopping {
                    task.status = TaskStatus::Running;
                }
                self.store.event(
                    Some(id),
                    "running",
                    &format!(
                        "Worker started: provider={:?} mode={:?} pid={pid:?}",
                        task.proposal.provider, task.proposal.mode
                    ),
                )?;
            }
            WorkerEvent::Output(output) => match output {
                ProviderOutput::Session(session) => {
                    self.store.record_managed_session(&session)?;
                    self.store.event(
                        Some(id),
                        "worker_debug",
                        &format!("Provider session={session}"),
                    )?;
                    task.session_id = Some(session);
                }
                ProviderOutput::Text(text) => {
                    self.store.event(Some(id), "output", &text)?;
                }
                ProviderOutput::Error(text) => {
                    self.store.event(Some(id), "provider_error", &text)?;
                }
                ProviderOutput::Usage { usd } => {
                    self.store.event(Some(id), "usage", &format!("Provider-reported cumulative estimate: ${usd:.6}; not a cross-provider spending cap"))?;
                }
                ProviderOutput::Completed { .. } => {}
            },
            WorkerEvent::Finished {
                success,
                interrupted,
                summary,
            } => {
                self.workers.remove(id);
                task.status = if interrupted {
                    TaskStatus::Interrupted
                } else if success {
                    TaskStatus::Succeeded
                } else {
                    TaskStatus::NeedsRetry
                };
                task.pid = None;
                task.summary = summary.chars().take(32_768).collect();
                self.store.event(Some(id), "finished", &task.summary)?;
            }
        }
        task.updated_at = now();
        self.store.save_task(&task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissed_results_preserve_worktree_and_cannot_dismiss_an_active_merge() {
        let (_dir, mut actor, workspace) = preparing_actor();
        std::fs::create_dir_all(&workspace.path).unwrap();
        std::fs::write(workspace.path.join("result.txt"), "keep my changes").unwrap();
        let mut source = actor.store.task("preparing").unwrap();
        source.worktree = Some(workspace.path.clone());
        source.branch = Some(workspace.branch);
        source.base_commit = Some(workspace.base_commit);
        source.status = TaskStatus::Succeeded;
        source.review = ReviewStatus::AwaitingReview;
        source.summary = "Completed useful changes".into();
        actor.store.save_task(&source).unwrap();
        actor.workers.clear();
        actor.preparing.clear();
        let mut job: TaskRecord =
            serde_json::from_value(actor.request_merge(&source.id).unwrap()).unwrap();
        assert!(
            actor
                .control(Request::Reject {
                    id: source.id.clone()
                })
                .is_err()
        );
        job.summary = "Integration blocked: main has overlapping edits in src/app.rs".into();
        actor.store.save_task(&job).unwrap();
        actor
            .merge_verified(
                &job.id,
                Err("Task worktree still has uncommitted changes".into()),
            )
            .unwrap();
        let failed = actor.store.task(&job.id).unwrap();
        assert!(
            failed
                .summary
                .starts_with("Integration blocked: main has overlapping edits")
        );
        assert!(
            failed
                .summary
                .contains("Git verification: Task worktree still has uncommitted changes")
        );
        actor
            .control(Request::Reject {
                id: source.id.clone(),
            })
            .unwrap();
        let dismissed = actor.store.task(&source.id).unwrap();
        assert_eq!(dismissed.status, TaskStatus::Rejected);
        assert_eq!(dismissed.review, ReviewStatus::Rejected);
        assert_eq!(dismissed.summary, source.summary);
        assert_eq!(dismissed.worktree, source.worktree);
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("result.txt")).unwrap(),
            "keep my changes"
        );
        assert!(actor.request_merge(&source.id).is_err());
    }

    #[test]
    fn background_merge_is_explicit_deduplicated_and_completes_atomically() {
        let (_dir, mut actor, workspace) = preparing_actor();
        let mut source = actor.store.task("preparing").unwrap();
        source.worktree = Some(workspace.path);
        source.branch = Some(workspace.branch);
        source.base_commit = Some(workspace.base_commit);
        source.status = TaskStatus::Succeeded;
        source.review = ReviewStatus::AwaitingReview;
        actor.store.save_task(&source).unwrap();
        actor.workers.clear();
        actor.preparing.clear();
        let job: TaskRecord =
            serde_json::from_value(actor.request_merge(&source.id).unwrap()).unwrap();
        assert_eq!(job.proposal.mode, TaskMode::Merge);
        assert_eq!(job.proposal.dependencies, vec![source.id.clone()]);
        assert_eq!(job.worktree, source.worktree);
        assert_eq!(job.status, TaskStatus::Queued);
        assert!(actor.request_merge(&source.id).is_err());
        assert_eq!(
            actor.store.task(&source.id).unwrap().review,
            ReviewStatus::AwaitingReview
        );
        actor
            .merge_verified(&job.id, Err("Commit is not merged".into()))
            .unwrap();
        assert_eq!(
            actor.store.task(&source.id).unwrap().review,
            ReviewStatus::AwaitingReview
        );
        assert_eq!(
            actor.store.task(&job.id).unwrap().status,
            TaskStatus::NeedsRetry
        );
        actor.request_merge(&source.id).unwrap();
        let retried = actor.store.task(&job.id).unwrap();
        assert_eq!(retried.worktree, source.worktree);
        assert_eq!(retried.attempts.len(), 1);
        actor
            .merge_verified(&job.id, Ok("Verified commit".into()))
            .unwrap();
        assert_eq!(
            actor.store.task(&source.id).unwrap().review,
            ReviewStatus::Integrated
        );
        assert_eq!(
            actor.store.task(&job.id).unwrap().status,
            TaskStatus::Succeeded
        );
    }

    #[test]
    fn retry_reservation_charges_each_attempt_once() {
        let (_dir, mut actor, _) = preparing_actor();
        let mut task = actor.store.task("preparing").unwrap();
        assert_eq!(actor.store.starts_today().unwrap(), 1);
        task.attempts.push(TaskAttempt {
            status: TaskStatus::NeedsRetry,
            session_id: None,
            worktree: None,
            branch: None,
            base_commit: None,
            summary: "stream failure".into(),
            started_at: None,
            finished_at: now(),
        });
        task.status = TaskStatus::Queued;
        actor.store.save_task(&task).unwrap();
        actor.store.reserve(&mut task).unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().status,
            TaskStatus::Starting
        );
        assert_eq!(actor.store.starts_today().unwrap(), 2);
        assert!(actor.store.reserve(&mut task).is_err());
        assert_eq!(actor.store.starts_today().unwrap(), 2);
    }

    #[test]
    fn debug_events_are_persisted_without_changing_task_state() {
        let (_dir, mut actor, _) = preparing_actor();
        let before = serde_json::to_value(actor.store.task("preparing").unwrap()).unwrap();
        actor
            .worker_event(
                "preparing",
                WorkerEvent::Diagnostic("stdout bytes=2200000".into()),
            )
            .unwrap();
        actor
            .planner_event(
                "plan-debug",
                WorkerEvent::Diagnostic("provider exited".into()),
            )
            .unwrap();
        assert_eq!(
            before,
            serde_json::to_value(actor.store.task("preparing").unwrap()).unwrap()
        );
        let events = actor.store.events(0).unwrap();
        assert!(events.iter().any(|e| e.kind == "worker_debug"
            && e.task_id.as_deref() == Some("preparing")
            && e.message == "stdout bytes=2200000"));
        assert!(
            events
                .iter()
                .any(|e| e.kind == "planner_debug" && e.message.contains("plan=plan-debug"))
        );
    }

    fn preparing_actor() -> (tempfile::TempDir, Actor, workspace::Workspace) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("state.sqlite3")).unwrap();
        let project = Project {
            use_worktrees: true,
            path: directory.path().join("project"),
            objective: "Improve parser".into(),
            objective_version: 1,
            enabled: true,
        };
        store.save_project(&project).unwrap();
        let mut task = TaskRecord {
            attempts: Vec::new(),
            automatic: false,
            started_at: None,
            id: "preparing".into(),
            proposal: TaskProposal {
                brief: None,
                risk: RiskLevel::Medium,
                project: project.path,
                objective_version: 1,
                prompt: "Improve parser errors".into(),
                rationale: "Useful independent work".into(),
                provider: Provider::Mock,
                mode: TaskMode::Edit,
                dependencies: vec![],
                expected_files: vec!["src/parser.rs".into()],
                completion_criteria: "Parser tests pass".into(),
            },
            status: TaskStatus::Queued,
            review: ReviewStatus::NotRequired,
            session_id: None,
            worktree: None,
            branch: None,
            base_commit: None,
            pid: None,
            summary: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        store.save_task(&task).unwrap();
        store.reserve(&mut task).unwrap();
        let (sender, _) = mpsc::channel(32);
        let (placeholder, _) = oneshot::channel();
        let actor = Actor {
            store,
            state_dir: directory.path().into(),
            workers: HashMap::from([("preparing".into(), placeholder)]),
            sender,
            shutting_down: false,
            planning: HashMap::new(),
            preparing: HashMap::from([("preparing".into(), false)]),
            observation: Observation::default(),
            observing: false,
            repositories: HashMap::new(),
            checking_repositories: Default::default(),
        };
        let workspace = workspace::Workspace {
            path: directory.path().join("preserved-worktree"),
            branch: "prodex/preparing".into(),
            base_commit: "base".into(),
        };
        (directory, actor, workspace)
    }

    #[test]
    fn preparation_rechecks_pause_stop_objective_and_enabled_state() {
        for action in [
            "pause",
            "pause_resume",
            "stop",
            "objective",
            "disable",
            "shutdown",
        ] {
            let (_directory, mut actor, workspace) = preparing_actor();
            match action {
                "pause" => actor.pause(true).unwrap(),
                "pause_resume" => {
                    actor.pause(true).unwrap();
                    actor.pause(false).unwrap();
                }
                "stop" => actor.stop("preparing").unwrap(),
                "objective" | "disable" => {
                    let mut project = actor.store.projects().unwrap().remove(0);
                    if action == "objective" {
                        project.objective_version += 1;
                    } else {
                        project.enabled = false;
                    }
                    actor.store.save_project(&project).unwrap();
                }
                "shutdown" => actor.begin_shutdown().unwrap(),
                _ => unreachable!(),
            }
            let expected_path = workspace.path.clone();
            actor.prepared("preparing", Ok(workspace)).unwrap();
            let task = actor.store.task("preparing").unwrap();
            assert_eq!(task.status, TaskStatus::Interrupted, "{action}");
            assert_eq!(task.worktree, Some(expected_path), "{action}");
            assert_eq!(task.branch.as_deref(), Some("prodex/preparing"));
            assert_eq!(task.base_commit.as_deref(), Some("base"));
            assert!(actor.workers.is_empty());
            assert!(actor.preparing.is_empty());
            assert!(task.session_id.is_none());
        }
    }

    #[test]
    fn cancellation_does_not_depend_on_database_health() {
        let (directory, mut actor, _) = preparing_actor();
        let (cancel, mut receive_cancel) = oneshot::channel();
        actor.workers.insert("running".into(), cancel);
        let db = rusqlite::Connection::open(directory.path().join("state.sqlite3")).unwrap();
        db.execute_batch("DROP TABLE tasks").unwrap();
        assert!(actor.store.tasks().is_err());
        actor.cancel_live();
        assert_eq!(receive_cancel.try_recv(), Ok(()));
        assert_eq!(actor.preparing.get("preparing"), Some(&true));
    }

    #[test]
    fn explicit_retry_preserves_attempts_and_rechecks_policy() {
        let (_directory, mut actor, workspace) = preparing_actor();
        actor
            .prepared("preparing", Err("not a git repository".into()))
            .unwrap();
        let mut task = actor.store.task("preparing").unwrap();
        assert_eq!(task.status, TaskStatus::NeedsRetry);
        std::fs::create_dir_all(&workspace.path).unwrap();
        std::fs::write(workspace.path.join("partial.txt"), "preserve this work").unwrap();
        task.worktree = Some(workspace.path.clone());
        task.session_id = Some("old-session".into());
        task.branch = Some(workspace.branch.clone());
        task.base_commit = Some(workspace.base_commit.clone());
        actor.store.save_task(&task).unwrap();
        actor.pause(true).unwrap();
        assert!(
            actor
                .control(Request::Retry {
                    id: task.id.clone()
                })
                .is_err()
        );
        assert!(actor.store.task(&task.id).unwrap().attempts.is_empty());
        actor.pause(false).unwrap();
        actor
            .control(Request::Retry {
                id: task.id.clone(),
            })
            .unwrap();
        let retried = actor.store.task(&task.id).unwrap();
        assert_eq!(retried.status, TaskStatus::Queued);
        assert_eq!(retried.attempts.len(), 1);
        assert_eq!(retried.attempts[0].worktree, Some(workspace.path.clone()));
        assert_eq!(
            retried.attempts[0].session_id.as_deref(),
            Some("old-session")
        );
        assert!(retried.attempts[0].summary.contains("not a git repository"));
        assert!(retried.worktree.is_none() && retried.session_id.is_none());
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("partial.txt")).unwrap(),
            "preserve this work"
        );
        assert!(actor.control(Request::Retry { id: task.id }).is_err());
    }

    #[test]
    fn provider_failure_waits_for_explicit_retry_and_can_be_rejected() {
        let (_directory, mut actor, _) = preparing_actor();
        actor.preparing.clear();
        actor
            .worker_event(
                "preparing",
                WorkerEvent::Finished {
                    success: false,
                    interrupted: false,
                    summary: "Authentication required".into(),
                },
            )
            .unwrap();
        let task = actor.store.task("preparing").unwrap();
        assert_eq!(task.status, TaskStatus::NeedsRetry);
        assert!(!task.status.occupies_slot());
        assert!(actor.workers.is_empty());
        actor.control(Request::Reject { id: task.id }).unwrap();
        assert_eq!(
            actor.store.task("preparing").unwrap().status,
            TaskStatus::Rejected
        );
    }

    #[test]
    fn failed_preparation_releases_ownership_and_does_not_launch() {
        let (_directory, mut actor, _) = preparing_actor();
        actor
            .prepared("preparing", Err("git failed".into()))
            .unwrap();
        assert_eq!(
            actor.store.task("preparing").unwrap().status,
            TaskStatus::NeedsRetry
        );
        assert!(actor.workers.is_empty());
        assert!(actor.preparing.is_empty());
    }

    fn activation_actor() -> (tempfile::TempDir, Actor, Vec<PathBuf>) {
        let (directory, mut actor, _) = preparing_actor();
        // A separate store avoids reusing the preparing-task fixture's project.
        actor.store = Store::open(&directory.path().join("activation.sqlite3")).unwrap();
        actor.workers.clear();
        actor.preparing.clear();
        let paths: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|name| {
                let path = directory.path().join(name);
                std::fs::create_dir(&path).unwrap();
                let path = path.canonicalize().unwrap();
                actor
                    .store
                    .save_project(&Project {
                        use_worktrees: true,
                        path: path.clone(),
                        objective: "Build useful work".into(),
                        objective_version: 1,
                        enabled: false,
                    })
                    .unwrap();
                path
            })
            .collect();
        (directory, actor, paths)
    }

    #[tokio::test]
    async fn check_now_skips_only_cooldown_and_preserves_observation_gating() {
        let (_dir, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        let mut settings = actor.store.settings().unwrap();
        settings.planner_provider = Provider::Mock;
        actor.store.save_settings(&settings).unwrap();
        actor.repositories.insert(
            paths[0].clone(),
            (now(), Ok(workspace::RepositoryState::Ready)),
        );
        let project = actor.store.project(&paths[0]).unwrap();
        actor.store.record_plan("recent", &project.path).unwrap();
        assert!(actor.planning_eligible(&project).is_err());
        assert!(
            actor
                .planning_eligible_with_cooldown(&project, true)
                .is_ok()
        );
        assert!(
            actor
                .control(Request::CheckNow {
                    project: project.path.clone()
                })
                .is_err()
        );
        observe_fixture(&mut actor, &project.path);
        settings.max_plans_per_day = 1;
        actor.store.save_settings(&settings).unwrap();
        let snapshot = actor.snapshot().unwrap();
        assert!(snapshot.planning_activity[0].daily_limit_reached);
        assert_eq!(
            snapshot.planning_activity[0].message,
            "Daily planning limit reached"
        );
        let error = actor
            .control(Request::CheckNow {
                project: project.path.clone(),
            })
            .unwrap_err()
            .to_string();
        assert!(error.contains("daily planning limit reached (1/1 checks)"));
        assert!(!error.contains("cooldown"));
        assert!(
            actor
                .control(Request::CheckNow {
                    project: project.path.clone()
                })
                .is_err()
        );
        settings.max_plans_per_day = 10;
        settings.paused = true;
        actor.store.save_settings(&settings).unwrap();
        assert!(
            actor
                .control(Request::CheckNow {
                    project: project.path.clone()
                })
                .is_err()
        );
        settings.paused = false;
        actor.store.save_settings(&settings).unwrap();
        actor
            .control(Request::CheckNow {
                project: project.path.clone(),
            })
            .unwrap();
        assert!(actor.planning.values().all(|p| p.automatic));
        assert_eq!(actor.planning.len(), 1);
        assert!(
            actor
                .control(Request::CheckNow {
                    project: project.path
                })
                .is_err()
        );
    }

    #[test]
    fn unconnect_stops_only_this_project_and_keeps_history_and_settings() {
        let (_dir, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        let (_fixture, fixture_actor, _) = preparing_actor();
        let template = fixture_actor.store.task("preparing").unwrap();
        let mut receivers = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let mut task = template.clone();
            task.id = format!("live-{index}");
            task.proposal.project = path.clone();
            task.status = TaskStatus::Running;
            actor.store.save_task(&task).unwrap();
            let (send, receive) = oneshot::channel();
            actor.workers.insert(task.id, send);
            receivers.push(receive);
        }
        let mut pending = template.clone();
        pending.id = "pending".into();
        pending.proposal.project = paths[0].clone();
        pending.status = TaskStatus::AwaitingApproval;
        actor.store.save_task(&pending).unwrap();
        actor
            .control(Request::UnconnectProject {
                project: paths[0].clone(),
            })
            .unwrap();
        assert!(receivers[0].try_recv().is_ok());
        assert!(matches!(
            receivers[1].try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(
            actor.store.task("live-0").unwrap().status,
            TaskStatus::Stopping
        );
        assert_eq!(
            actor.store.task("live-1").unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            actor.store.task("pending").unwrap().status,
            TaskStatus::Interrupted
        );
        assert!(!actor.store.project(&paths[0]).unwrap().enabled);
        assert!(paths[0].is_dir());
        let settings = actor.store.settings().unwrap();
        assert!(settings.planning_enabled && !settings.paused);
        assert_eq!(settings.project_scope, Some(vec![paths[1].clone()]));
        assert_eq!(actor.store.tasks().unwrap().len(), 3);
    }

    #[test]
    fn activation_on_off_and_empty_selection_are_distinct() {
        let (_directory, mut actor, paths) = activation_actor();
        assert!(
            actor
                .control(Request::SetActivation {
                    enabled: true,
                    projects: None
                })
                .is_err()
        );
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        let state = actor.store.snapshot().unwrap();
        assert!(!state.settings.paused && state.settings.planning_enabled);
        assert!(state.projects.iter().all(|project| project.enabled));
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(vec![]),
            })
            .unwrap();
        let state = actor.store.snapshot().unwrap();
        assert!(!state.settings.paused && state.settings.planning_enabled);
        assert_eq!(state.settings.project_scope, Some(vec![]));
        assert!(state.projects.iter().all(|project| !project.enabled));
        actor
            .control(Request::SetActivation {
                enabled: false,
                projects: Some(paths.clone()),
            })
            .unwrap();
        let settings = actor.store.settings().unwrap();
        assert!(settings.paused && !settings.planning_enabled);
        assert_eq!(settings.project_scope, Some(paths));
    }

    #[test]
    fn activation_scope_persists_and_applies_to_new_projects() {
        let (directory, mut actor, paths) = activation_actor();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(vec![paths[0].clone()]),
            })
            .unwrap();
        let path = directory.path().join("new-project");
        std::fs::create_dir(&path).unwrap();
        actor
            .control(Request::Project {
                path: path.clone(),
                objective: "New objective".into(),
                enabled: true,
            })
            .unwrap();
        assert!(
            actor
                .store
                .project(&path.canonicalize().unwrap())
                .unwrap()
                .enabled
        );
        let reopened = Store::open(&directory.path().join("activation.sqlite3")).unwrap();
        assert_eq!(
            reopened.settings().unwrap().project_scope,
            Some(vec![path.canonicalize().unwrap(), paths[0].clone()])
        );
        actor
            .control(Request::Project {
                path: paths[0].clone(),
                objective: "Build useful work".into(),
                enabled: false,
            })
            .unwrap();
        assert!(!actor.store.project(&paths[0]).unwrap().enabled);
        // A stale settings form must not reset selection.
        actor
            .control(Request::Configure {
                settings: Settings::default(),
            })
            .unwrap();
        assert_eq!(
            actor.store.settings().unwrap().project_scope,
            Some(vec![path.canonicalize().unwrap()])
        );
    }

    #[test]
    fn invalid_activation_does_not_mutate_or_cancel() {
        let (directory, mut actor, _) = activation_actor();
        let before = serde_json::to_value(actor.store.snapshot().unwrap()).unwrap();
        let (cancel, mut receiver) = oneshot::channel();
        actor.workers.insert("running".into(), cancel);
        let unknown = directory.path().join("unknown");
        std::fs::create_dir(&unknown).unwrap();
        assert!(
            actor
                .control(Request::SetActivation {
                    enabled: false,
                    projects: Some(vec![unknown])
                })
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_value(actor.store.snapshot().unwrap()).unwrap()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn activation_scope_stops_excluded_work_and_cancels_planner() {
        let (_directory, mut actor, paths) = activation_actor();
        let (_fixture, fixture_actor, _) = preparing_actor();
        let template = fixture_actor.store.task("preparing").unwrap();
        let mut receivers = Vec::new();
        for (id, project, status) in [
            ("included", &paths[0], TaskStatus::Running),
            ("excluded", &paths[1], TaskStatus::Running),
            ("queued", &paths[1], TaskStatus::Queued),
        ] {
            let mut task = template.clone();
            task.id = id.into();
            task.proposal.project = project.clone();
            task.status = status;
            actor.store.save_task(&task).unwrap();
            if status == TaskStatus::Running {
                let (cancel, receiver) = oneshot::channel();
                actor.workers.insert(id.into(), cancel);
                receivers.push(receiver);
            }
        }
        let (cancel, mut planner_cancel) = oneshot::channel();
        actor.planning.insert(
            "plan".into(),
            Planning {
                automatic: false,
                project: actor.store.project(&paths[0]).unwrap(),
                worker_provider: Provider::Mock,
                cancel: Some(cancel),
                cancelled: false,
            },
        );
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(vec![paths[0].clone()]),
            })
            .unwrap();
        assert!(matches!(
            receivers[0].try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(receivers[1].try_recv(), Ok(()));
        assert_eq!(planner_cancel.try_recv(), Ok(()));
        assert_eq!(
            actor.store.task("excluded").unwrap().status,
            TaskStatus::Stopping
        );
        assert_eq!(
            actor.store.task("queued").unwrap().status,
            TaskStatus::Interrupted
        );
        actor
            .control(Request::SetActivation {
                enabled: false,
                projects: Some(paths.clone()),
            })
            .unwrap();
        assert_eq!(receivers[0].try_recv(), Ok(()));
        assert_eq!(
            actor.store.task("included").unwrap().status,
            TaskStatus::Stopping
        );
    }

    #[test]
    fn visible_suggestions_refill_when_busy_but_reserve_respects_threshold() {
        let (_directory, mut actor, paths) = activation_actor();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        let project = actor.store.project(&paths[0]).unwrap();
        let (_fixture, fixture_actor, _) = preparing_actor();
        let template = fixture_actor.store.task("preparing").unwrap();
        for index in 0..2 {
            let mut task = template.clone();
            task.id = format!("active-{index}");
            task.status = TaskStatus::Running;
            task.proposal.project = paths[0].clone();
            actor.store.save_task(&task).unwrap();
        }
        // No pending ideas: refill is eligible even with workers occupying slots.
        assert!(actor.planning_eligible(&project).is_ok());
        for index in 0..3 {
            let mut task = template.clone();
            task.id = format!("idea-{index}");
            task.status = TaskStatus::AwaitingApproval;
            task.proposal.project = paths[0].clone();
            actor.store.save_task(&task).unwrap();
        }
        assert!(actor.planning_eligible(&project).is_err());
        // Explicit Plan uses the same threshold, not a bypass.
        assert!(
            actor
                .control(Request::Plan {
                    project: paths[0].clone()
                })
                .is_err()
        );
        let mut completed = actor.store.task("active-0").unwrap();
        completed.status = TaskStatus::Succeeded;
        actor.store.save_task(&completed).unwrap();
        assert!(actor.planning_eligible(&project).is_ok());
        let mut settings = actor.store.settings().unwrap();
        settings.max_concurrent = 1;
        settings.max_per_project = 1;
        actor.store.save_settings(&settings).unwrap();
        assert!(actor.planning_eligible(&project).is_err());
        let mut completed = actor.store.task("active-1").unwrap();
        completed.status = TaskStatus::Succeeded;
        actor.store.save_task(&completed).unwrap();
        assert!(actor.planning_eligible(&project).is_ok());
        settings.planning_free_slots = 0;
        assert!(actor.store.save_settings(&settings).is_err());
        settings.planning_free_slots = 33;
        assert!(actor.store.save_settings(&settings).is_err());
    }

    #[test]
    fn ranked_backlog_survives_reload_and_refills_without_worker_capacity() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        let project = actor.store.project(&paths[0]).unwrap();
        let (_fixture, fixture_actor, _) = preparing_actor();
        let mut active = fixture_actor.store.task("preparing").unwrap();
        active.proposal.project = project.path.clone();
        active.status = TaskStatus::Running;
        let mut settings = actor.store.settings().unwrap();
        settings.max_concurrent = 1;
        settings.max_per_project = 1;
        actor.store.save_settings(&settings).unwrap();
        actor.store.save_task(&active).unwrap();
        assert!(actor.planning_eligible(&project).is_ok());
        for round in 0..2 {
            let id = format!("backlog-{round}");
            actor.planning.insert(
                id.clone(),
                Planning {
                    automatic: false,
                    project: project.clone(),
                    worker_provider: Provider::Mock,
                    cancel: None,
                    cancelled: false,
                },
            );
            actor.store.record_plan(&id, &project.path).unwrap();
            let proposals: Vec<_> = (0..10)
                .map(|rank| {
                    serde_json::json!({
                        "prompt":format!("Implement useful outcome {round}-{rank}"),
                        "rationale":"Fix a documented user journey",
                        "completion_criteria":"The journey passes its acceptance test",
                        "mode":"edit", "expected_files":[format!("src/feature-{round}-{rank}.rs")],
                        "dependencies":[], "risk":"low"
                    })
                })
                .collect();
            actor
                .planner_event(
                    &id,
                    WorkerEvent::Finished {
                        success: true,
                        interrupted: false,
                        summary: serde_json::json!({"proposals":proposals}).to_string(),
                    },
                )
                .unwrap();
            let tasks = actor.store.snapshot().unwrap().tasks;
            let pending: Vec<_> = tasks
                .iter()
                .filter(|t| t.status == TaskStatus::AwaitingApproval)
                .collect();
            assert_eq!(pending.len(), 10);
            if round == 0 {
                for (rank, task) in pending.iter().enumerate() {
                    assert_eq!(
                        task.proposal.prompt,
                        format!("Implement useful outcome 0-{rank}")
                    );
                }
                assert!(
                    actor
                        .planning_eligible_with_cooldown(&project, true)
                        .unwrap_err()
                        .to_string()
                        .contains("backlog is full")
                );
                for task in pending.iter().take(8) {
                    actor
                        .control(Request::Reject {
                            id: task.id.clone(),
                        })
                        .unwrap();
                }
                assert!(
                    actor
                        .planning_eligible_with_cooldown(&project, true)
                        .is_ok()
                );
            } else {
                // Existing reserve retains priority; new output fills only eight vacancies.
                assert_eq!(pending[0].proposal.prompt, "Implement useful outcome 0-8");
                assert_eq!(pending[1].proposal.prompt, "Implement useful outcome 0-9");
                assert_eq!(pending[9].proposal.prompt, "Implement useful outcome 1-7");
            }
        }
        assert_eq!(
            actor.store.task(&active.id).unwrap().status,
            TaskStatus::Running
        );
    }

    #[test]
    fn planner_discards_risk_above_current_ceiling_before_task_creation() {
        let (_directory, mut actor, paths) = activation_actor();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        let project = actor.store.project(&paths[0]).unwrap();
        let mut settings = actor.store.settings().unwrap();
        settings.max_proposal_risk = RiskLevel::Low;
        settings.auto_approve_read_only = true;
        actor.store.save_settings(&settings).unwrap();
        actor.planning.insert(
            "risk-plan".into(),
            Planning {
                automatic: false,
                project: project.clone(),
                worker_provider: Provider::Mock,
                cancel: None,
                cancelled: false,
            },
        );
        actor.store.record_plan("risk-plan", &project.path).unwrap();
        let proposal = |prompt: &str, risk: &str| serde_json::json!({"prompt":prompt,"rationale":"Useful independent research","completion_criteria":"Document the findings","mode":"edit","expected_files":["src/parser.rs"],"dependencies":[],"risk":risk});
        let summary = serde_json::json!({"proposals":[proposal("Sensitive investigation", "high"), proposal("Bounded investigation", "low"), proposal("Broader investigation", "medium")]}).to_string();
        actor
            .planner_event(
                "risk-plan",
                WorkerEvent::Finished {
                    success: true,
                    interrupted: false,
                    summary,
                },
            )
            .unwrap();
        let tasks = actor.store.tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].proposal.risk, RiskLevel::Low);
        assert_eq!(tasks[0].status, TaskStatus::AwaitingApproval);
        assert_eq!(
            actor
                .store
                .events(0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "proposal_discarded")
                .count(),
            2
        );
    }

    #[test]
    fn activation_scope_changes_preserve_explicit_pause_until_resume_or_off_on() {
        let (_directory, mut actor, paths) = activation_actor();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        actor.control(Request::Pause).unwrap();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(vec![paths[0].clone()]),
            })
            .unwrap();
        let settings = actor.store.settings().unwrap();
        assert!(settings.paused && settings.planning_enabled);
        assert_eq!(settings.project_scope, Some(vec![paths[0].clone()]));
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        assert!(actor.store.settings().unwrap().paused);
        actor.control(Request::Resume).unwrap();
        assert!(!actor.store.settings().unwrap().paused);
        actor.control(Request::Pause).unwrap();
        actor
            .control(Request::SetActivation {
                enabled: false,
                projects: Some(paths.clone()),
            })
            .unwrap();
        actor
            .control(Request::SetActivation {
                enabled: true,
                projects: Some(paths.clone()),
            })
            .unwrap();
        let settings = actor.store.settings().unwrap();
        assert!(!settings.paused && settings.planning_enabled);
    }

    fn observe_fixture(actor: &mut Actor, path: &Path) {
        actor.observation = Observation {
            checked_at: Some(now()),
            available: true,
            detail: "Fixture live Codex session".into(),
            sessions: vec![ObservedSession {
                id: "independent".into(),
                cwd: path.into(),
                project: path.into(),
                last_seen: now(),
                source: "fixture".into(),
            }],
        };
    }

    #[tokio::test]
    async fn main_folder_mode_skips_worktree_and_serializes_coding_until_review() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        actor
            .control(Request::SetWorktrees {
                project: paths[0].clone(),
                enabled: false,
            })
            .unwrap();
        let (_fixture, fixture, _) = preparing_actor();
        let mut first = fixture.store.task("preparing").unwrap();
        first.id = "direct-first".into();
        first.status = TaskStatus::Queued;
        first.proposal.project = paths[0].clone();
        first.proposal.expected_files = vec!["a.rs".into()];
        let mut second = first.clone();
        second.id = "direct-second".into();
        second.proposal.expected_files = vec!["b.rs".into()];
        actor.store.save_task(&first).unwrap();
        actor.store.save_task(&second).unwrap();
        actor.schedule().unwrap();
        let tasks = actor.store.tasks().unwrap();
        assert_eq!(tasks.iter().filter(|t| t.status.occupies_slot()).count(), 1);
        let started = tasks.iter().find(|t| t.status.occupies_slot()).unwrap();
        assert_eq!(started.proposal.mode, TaskMode::EditInPlace);
        assert_eq!(started.review, ReviewStatus::AwaitingReview);
        assert!(started.worktree.is_none());
        assert!(actor.preparing.is_empty());
        assert!(!paths[0].join(".git").exists());
        assert!(
            actor
                .control(Request::SetWorktrees {
                    project: paths[0].clone(),
                    enabled: true
                })
                .is_err()
        );
        assert!(
            actor
                .control(Request::MarkReviewed {
                    id: started.id.clone()
                })
                .is_err()
        );
        actor
            .worker_event(
                &started.id,
                WorkerEvent::Finished {
                    success: true,
                    interrupted: false,
                    summary: "Done".into(),
                },
            )
            .unwrap();
        actor
            .control(Request::MarkReviewed {
                id: started.id.clone(),
            })
            .unwrap();
        assert_eq!(
            actor.store.task(&started.id).unwrap().review,
            ReviewStatus::Integrated
        );
        actor
            .control(Request::SetWorktrees {
                project: paths[0].clone(),
                enabled: true,
            })
            .unwrap();
        assert!(actor.store.project(&paths[0]).unwrap().use_worktrees);
    }

    #[test]
    fn main_folder_selection_removes_only_git_setup_dependencies_and_survives_registration() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        observe_fixture(&mut actor, &paths[0]);
        actor.ensure_git_setup(&paths[0]).unwrap();
        let setup = actor.store.tasks().unwrap().remove(0);
        let (_fixture, fixture, _) = preparing_actor();
        let mut task = fixture.store.task("preparing").unwrap();
        task.status = TaskStatus::AwaitingApproval;
        task.proposal.project = paths[0].clone();
        task.proposal.dependencies = vec![setup.id.clone(), "keep-other-dependency".into()];
        actor.store.save_task(&task).unwrap();
        actor
            .control(Request::SetWorktrees {
                project: paths[0].clone(),
                enabled: false,
            })
            .unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().proposal.dependencies,
            vec!["keep-other-dependency"]
        );
        assert_eq!(
            actor.store.task(&setup.id).unwrap().status,
            TaskStatus::Interrupted
        );
        actor.ensure_git_setup(&paths[0]).unwrap();
        assert_eq!(actor.store.tasks().unwrap().len(), 2);
        actor
            .control(Request::Project {
                path: paths[0].clone(),
                objective: "Build useful work".into(),
                enabled: true,
            })
            .unwrap();
        assert!(!actor.store.project(&paths[0]).unwrap().use_worktrees);
        assert!(actor.store.project(&paths[1]).unwrap().use_worktrees);
    }

    #[test]
    fn worktree_integration_acknowledgement_requires_a_completed_worktree() {
        let (_directory, mut actor, workspace) = preparing_actor();
        assert!(
            actor
                .control(Request::ConfirmIntegrated {
                    id: "preparing".into()
                })
                .is_err()
        );
        actor.workers.clear();
        actor.preparing.clear();
        let mut task = actor.store.task("preparing").unwrap();
        task.status = TaskStatus::Succeeded;
        task.review = ReviewStatus::AwaitingReview;
        task.worktree = Some(workspace.path);
        actor.store.save_task(&task).unwrap();
        actor
            .control(Request::ConfirmIntegrated {
                id: task.id.clone(),
            })
            .unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().review,
            ReviewStatus::Integrated
        );
        assert!(
            actor
                .control(Request::MarkReviewed { id: task.id })
                .is_err()
        );
    }

    #[test]
    fn git_setup_is_deduplicated_requires_approval_and_blocks_coding() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        observe_fixture(&mut actor, &paths[0]);
        let (_fixture, fixture_actor, _) = preparing_actor();
        let mut coding = fixture_actor.store.task("preparing").unwrap();
        coding.proposal.project = paths[0].clone();
        coding.status = TaskStatus::NeedsRetry;
        actor.store.save_task(&coding).unwrap();
        for _ in 0..2 {
            actor
                .repository_checked(
                    paths[0].clone(),
                    Ok(workspace::RepositoryState::MissingRepository),
                )
                .unwrap();
        }
        let tasks = actor.store.tasks().unwrap();
        let setup = tasks
            .iter()
            .find(|t| t.proposal.mode == TaskMode::InitializeRepository)
            .unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(setup.status, TaskStatus::AwaitingApproval);
        assert!(setup.worktree.is_none());
        assert_eq!(
            actor.store.task(&coding.id).unwrap().proposal.dependencies,
            vec![setup.id.clone()]
        );
        assert_eq!(actor.store.starts_today().unwrap(), 0);
        assert_eq!(
            actor.snapshot().unwrap().planning_activity[0].message,
            "Git setup ready for approval"
        );
        actor
            .repository_checked(paths[0].clone(), Ok(workspace::RepositoryState::Ready))
            .unwrap();
        assert_eq!(
            actor.store.task(&setup.id).unwrap().status,
            TaskStatus::Succeeded
        );
        assert_eq!(
            actor.store.task(&coding.id).unwrap().status,
            TaskStatus::NeedsRetry
        );
    }

    #[test]
    fn setup_detection_respects_observation_and_rejection() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        actor.ensure_git_setup(&paths[0]).unwrap();
        assert!(actor.store.tasks().unwrap().is_empty());
        observe_fixture(&mut actor, &paths[0]);
        actor.ensure_git_setup(&paths[0]).unwrap();
        let mut setup = actor.store.tasks().unwrap().remove(0);
        setup.status = TaskStatus::Rejected;
        actor.store.save_task(&setup).unwrap();
        actor.ensure_git_setup(&paths[0]).unwrap();
        assert_eq!(actor.store.tasks().unwrap().len(), 1);
        assert_eq!(
            actor.store.task(&setup.id).unwrap().status,
            TaskStatus::Rejected
        );
    }

    #[test]
    fn git_setup_requires_a_verified_commit_and_never_reopens_ready_project() {
        let (_directory, mut actor, _) = preparing_actor();
        let mut task = actor.store.task("preparing").unwrap();
        task.proposal.mode = TaskMode::InitializeRepository;
        actor.store.save_task(&task).unwrap();
        actor
            .setup_prepared(&task.id, Ok(workspace::RepositoryState::Ready))
            .unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().status,
            TaskStatus::Succeeded
        );
        assert!(actor.workers.is_empty());
        task.status = TaskStatus::Running;
        actor.store.save_task(&task).unwrap();
        actor
            .setup_verified(
                &task.id,
                "Agent says done".into(),
                Ok(workspace::RepositoryState::MissingCommit),
            )
            .unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().status,
            TaskStatus::NeedsRetry
        );
        actor
            .setup_verified(
                &task.id,
                "Initial commit created".into(),
                Ok(workspace::RepositoryState::Ready),
            )
            .unwrap();
        assert_eq!(
            actor.store.task(&task.id).unwrap().status,
            TaskStatus::Succeeded
        );
    }

    #[test]
    fn selecting_and_enabling_without_a_session_does_not_plan() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths)).unwrap();
        actor.schedule_planning().unwrap();
        assert!(actor.planning.is_empty());
        assert!(
            !actor
                .store
                .events(0)
                .unwrap()
                .iter()
                .any(|e| e.kind == "planning_started")
        );
    }

    #[test]
    fn snapshot_explains_active_research_and_cooldown_without_process_details() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        observe_fixture(&mut actor, &paths[0]);
        let project = actor.store.project(&paths[0]).unwrap();
        actor
            .store
            .record_plan("visible-plan", &project.path)
            .unwrap();
        actor.planning.insert(
            "visible-plan".into(),
            Planning {
                automatic: true,
                project: project.clone(),
                worker_provider: Provider::Mock,
                cancel: None,
                cancelled: false,
            },
        );
        assert_eq!(
            actor.snapshot().unwrap().planning_activity[0].message,
            "Looking for work…"
        );
        actor
            .planner_event(
                "visible-plan",
                WorkerEvent::Finished {
                    success: false,
                    interrupted: true,
                    summary: "Worker interrupted".into(),
                },
            )
            .unwrap();
        let snapshot = actor.snapshot().unwrap();
        assert_eq!(
            snapshot.planning_activity[0].message,
            "Previous check stopped"
        );
        assert!(snapshot.planning_activity[0].next_check_at.unwrap() > now());
        assert!(
            !actor
                .store
                .plan_allowed(&project.path, &snapshot.settings)
                .unwrap()
        );
    }

    #[test]
    fn notebook_updates_on_abstention_but_not_invalid_failed_or_stale_results() {
        for case in ["abstain", "invalid", "failed", "cancelled", "stale"] {
            let (_directory, mut actor, paths) = activation_actor();
            actor.set_activation(true, Some(paths.clone())).unwrap();
            let project = actor.store.project(&paths[0]).unwrap();
            actor
                .store
                .save_project_notes(&project, "Previous evidence")
                .unwrap();
            actor
                .store
                .record_plan("notes-plan", &project.path)
                .unwrap();
            actor.planning.insert(
                "notes-plan".into(),
                Planning {
                    automatic: false,
                    project: project.clone(),
                    worker_provider: Provider::Mock,
                    cancel: None,
                    cancelled: case == "cancelled",
                },
            );
            if case == "stale" {
                let mut changed = project.clone();
                changed.objective_version += 1;
                actor.store.save_project(&changed).unwrap();
            }
            actor
                .planner_event(
                    "notes-plan",
                    WorkerEvent::Finished {
                        success: case != "failed",
                        interrupted: false,
                        summary: if case == "invalid" {
                            r#"{"project_notes":"New evidence","proposals":[{}]}"#.into()
                        } else {
                            r#"{"project_notes":"New evidence","proposals":[]}"#.into()
                        },
                    },
                )
                .unwrap();
            assert_eq!(
                actor
                    .store
                    .project_notes(&project.path)
                    .unwrap()
                    .unwrap()
                    .text,
                if case == "abstain" {
                    "New evidence"
                } else {
                    "Previous evidence"
                }
            );
        }
    }

    #[test]
    fn automatic_proposals_keep_their_gate_and_late_results_are_discarded() {
        for session_lost in [false, true] {
            let (_directory, mut actor, paths) = activation_actor();
            actor.set_activation(true, Some(paths.clone())).unwrap();
            observe_fixture(&mut actor, &paths[0]);
            let project = actor.store.project(&paths[0]).unwrap();
            actor.planning.insert(
                "auto-plan".into(),
                Planning {
                    automatic: true,
                    project: project.clone(),
                    worker_provider: Provider::Mock,
                    cancel: None,
                    cancelled: false,
                },
            );
            actor.store.record_plan("auto-plan", &project.path).unwrap();
            if session_lost {
                actor.observation = Observation::default();
            }
            let summary = serde_json::json!({"project_notes":"Goal: a usable parser. Empty input still fails; verify the proposed fix before marking complete.","proposals":[{"prompt":"Investigate parser edge cases", "rationale":"Independent useful research", "completion_criteria":"Report concrete findings", "mode":"edit", "expected_files":["src/parser.rs"], "dependencies":[], "risk":"low"}]}).to_string();
            actor
                .planner_event(
                    "auto-plan",
                    WorkerEvent::Finished {
                        success: true,
                        interrupted: false,
                        summary,
                    },
                )
                .unwrap();
            let tasks = actor.store.tasks().unwrap();
            assert_eq!(
                actor.store.project_notes(&project.path).unwrap().is_some(),
                !session_lost
            );
            if session_lost {
                assert!(tasks.is_empty());
            } else {
                assert_eq!(tasks.len(), 1);
                assert!(tasks[0].automatic);
                actor
                    .control(Request::Approve {
                        id: tasks[0].id.clone(),
                    })
                    .unwrap();
                actor
                    .observed(Observation {
                        checked_at: Some(now()),
                        available: true,
                        detail: "No live sessions".into(),
                        sessions: vec![],
                    })
                    .unwrap();
                actor.schedule().unwrap();
                assert_eq!(
                    actor.store.task(&tasks[0].id).unwrap().status,
                    TaskStatus::Queued
                );
                assert_eq!(actor.store.starts_today().unwrap(), 0);
            }
        }
    }

    #[test]
    fn session_loss_cancels_research_but_not_running_workers() {
        let (_directory, mut actor, paths) = activation_actor();
        actor.set_activation(true, Some(paths.clone())).unwrap();
        observe_fixture(&mut actor, &paths[0]);
        let (cancel_plan, mut plan_receive) = oneshot::channel();
        let (cancel_worker, mut worker_receive) = oneshot::channel();
        actor.workers.insert("running".into(), cancel_worker);
        actor.planning.insert(
            "auto-plan".into(),
            Planning {
                automatic: true,
                project: actor.store.project(&paths[0]).unwrap(),
                worker_provider: Provider::Mock,
                cancel: Some(cancel_plan),
                cancelled: false,
            },
        );
        actor
            .observed(Observation {
                checked_at: Some(now()),
                available: true,
                detail: "No live sessions".into(),
                sessions: vec![],
            })
            .unwrap();
        assert_eq!(plan_receive.try_recv(), Ok(()));
        assert!(matches!(
            worker_receive.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn planner_session_ownership_survives_restart_for_observer_exclusion() {
        let (directory, mut actor, _) = activation_actor();
        actor
            .planner_event(
                "plan",
                WorkerEvent::Output(crate::providers::ProviderOutput::Session(
                    "managed-plan-session".into(),
                )),
            )
            .unwrap();
        let reopened = Store::open(&directory.path().join("activation.sqlite3")).unwrap();
        assert_eq!(
            reopened.managed_sessions().unwrap(),
            vec!["managed-plan-session"]
        );
    }

    #[test]
    fn automatic_preparation_rechecks_observation_before_provider_launch() {
        let (_directory, mut actor, workspace) = preparing_actor();
        let mut task = actor.store.task("preparing").unwrap();
        task.automatic = true;
        actor.store.save_task(&task).unwrap();
        let mut settings = actor.store.settings().unwrap();
        settings.planning_enabled = true;
        settings.project_scope = Some(vec![task.proposal.project.clone()]);
        actor.store.save_settings(&settings).unwrap();
        actor.prepared("preparing", Ok(workspace)).unwrap();
        let task = actor.store.task("preparing").unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        assert!(task.worktree.is_some());
        assert!(task.session_id.is_none());
        assert!(task.summary.contains("live Codex session"));
    }
}
