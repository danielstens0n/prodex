use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use prodex_core::{ipc, model::*, service};
use std::{path::PathBuf, process::Stdio};

#[derive(Parser)]
#[command(version, about = "Local coordinator for Codex and Claude Code")]
struct Cli {
    /// Service data directory. Also accepts PRODEX_STATE_DIR.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Agent {
    Codex,
    Claude,
    Mock,
}
impl From<Agent> for Provider {
    fn from(agent: Agent) -> Self {
        match agent {
            Agent::Codex => Self::Codex,
            Agent::Claude => Self::Claude,
            Agent::Mock => Self::Mock,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Run the service in the foreground (use start to detach).
    Daemon,
    /// Start the service independently of this terminal.
    Start,
    Status,
    /// Configure a project and its user-approved objective.
    Project {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        disabled: bool,
    },
    /// Submit a task for approval; --approve authorizes execution now.
    Submit {
        prompt: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, value_enum)]
        provider: Option<Agent>,
        #[arg(long)]
        edit: bool,
        #[arg(long)]
        approve: bool,
        #[arg(long, default_value = "User-requested task")]
        rationale: String,
        #[arg(
            long,
            default_value = "Report findings, changes, and validation evidence"
        )]
        completion: String,
        #[arg(long)]
        file: Vec<String>,
        #[arg(long)]
        depends_on: Vec<String>,
    },
    Approve {
        id: String,
    },
    Reject {
        id: String,
    },
    Pause,
    Resume,
    /// Find useful coding work now; proposals require approval.
    Plan {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    Stop {
        id: Option<String>,
        #[arg(long, conflicts_with = "id")]
        all: bool,
    },
    /// Stop workers and shut down, preserving history/worktrees.
    Shutdown,
    Events {
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long)]
        follow: bool,
    },
    Inspect {
        id: String,
    },
    /// Print a native resume command; never attach to a running worker.
    Open {
        id: String,
    },
    Configure {
        #[arg(long)]
        max_concurrent: Option<usize>,
        #[arg(long)]
        max_per_project: Option<usize>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        max_starts_per_day: Option<usize>,
        #[arg(long, value_enum)]
        provider: Option<Agent>,
        #[arg(long)]
        planning_enabled: Option<bool>,
        #[arg(long, value_enum)]
        planner_provider: Option<Agent>,
        #[arg(long)]
        planner_cooldown_secs: Option<u64>,
        #[arg(long)]
        max_plans_per_day: Option<usize>,
        #[arg(long)]
        auto_approve_read_only: Option<bool>,
    },
    ResolveRecovery {
        id: String,
        #[arg(long)]
        confirmed_stopped: bool,
    },
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("PRODEX_STATE_DIR") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME").context("set --state-dir or PRODEX_STATE_DIR")?;
    Ok(PathBuf::from(home).join(".local/state/prodex"))
}

async fn send(dir: &std::path::Path, request: Request) -> Result<serde_json::Value> {
    let response = ipc::request(dir, request).await?;
    if !response.ok {
        bail!(
            "{}",
            response.error.unwrap_or_else(|| "request failed".into())
        );
    }
    Ok(response.data)
}
async fn snapshot(dir: &std::path::Path) -> Result<Snapshot> {
    Ok(serde_json::from_value(send(dir, Request::Status).await?)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = match cli.state_dir {
        Some(dir) => dir,
        None => default_state_dir()?,
    };
    let dir = if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir()?.join(dir)
    };
    let request = match cli.command {
        Command::Daemon => return service::serve(&dir).await,
        Command::Start => {
            if ipc::request(&dir, Request::Status).await.is_ok() {
                println!("Prodex is already running");
                return Ok(());
            }
            std::fs::create_dir_all(&dir)?;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            let log = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(dir.join("daemon.log"))?;
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .arg("--state-dir")
                .arg(&dir)
                .arg("daemon")
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .process_group(0)
                .spawn()?;
            for _ in 0..50 {
                if ipc::request(&dir, Request::Status).await.is_ok() {
                    println!("Prodex started (state: {})", dir.display());
                    return Ok(());
                }
                if let Some(status) = child.try_wait()? {
                    bail!(
                        "daemon exited {status}; inspect {}",
                        dir.join("daemon.log").display()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            bail!(
                "daemon did not become ready; inspect {}",
                dir.join("daemon.log").display()
            );
        }
        Command::Status => {
            let state = snapshot(&dir).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                println!(
                    "Prodex {} · {} active / {} allowed",
                    if state.settings.paused {
                        "paused"
                    } else {
                        "ready"
                    },
                    state
                        .tasks
                        .iter()
                        .filter(|t| t.status.occupies_slot())
                        .count(),
                    state.settings.max_concurrent
                );
                println!("Observation: {}", state.observation.detail);
                for project in state.projects {
                    let presence = if !project.enabled {
                        "not allowed"
                    } else if state
                        .observation
                        .sessions
                        .iter()
                        .any(|s| s.project == project.path)
                    {
                        "Codex observed"
                    } else {
                        "waiting for Codex"
                    };
                    println!(
                        "Project {} [{}]: {}",
                        project.path.display(),
                        presence,
                        project.objective
                    );
                }
                for task in state.tasks {
                    println!(
                        "{}  {:?}  {:?}  {}",
                        task.id,
                        task.proposal.provider,
                        task.status,
                        task.proposal.prompt.lines().next().unwrap_or_default()
                    );
                }
            }
            return Ok(());
        }
        Command::Project {
            path,
            objective,
            disabled,
        } => Request::Project {
            path: path.canonicalize()?,
            objective,
            enabled: !disabled,
        },
        Command::Submit {
            prompt,
            project,
            provider,
            edit,
            approve,
            rationale,
            completion,
            file,
            depends_on,
        } => {
            let state = snapshot(&dir).await?;
            let project = project.canonicalize()?;
            let configured =
                state.projects.iter().find(|p| p.path == project).context(
                    "configure this project first with `prodex project --objective ...`",
                )?;
            Request::Submit {
                proposal: TaskProposal {
                    brief: None,
                    project,
                    objective_version: configured.objective_version,
                    risk: RiskLevel::Medium,
                    prompt,
                    rationale,
                    provider: provider
                        .map(Into::into)
                        .unwrap_or(state.settings.preferred_provider),
                    mode: if edit
                        || provider
                            .map(Into::into)
                            .unwrap_or(state.settings.preferred_provider)
                            != Provider::Mock
                    {
                        TaskMode::Edit
                    } else {
                        TaskMode::ReadOnly
                    },
                    dependencies: depends_on,
                    expected_files: file,
                    completion_criteria: completion,
                },
                approved: approve,
            }
        }
        Command::Approve { id } => Request::Approve { id },
        Command::Reject { id } => Request::Reject { id },
        Command::Pause => Request::Pause,
        Command::Resume => Request::Resume,
        Command::Plan { project } => Request::Plan {
            project: project.canonicalize()?,
        },
        Command::Stop { id, all } => {
            if all {
                Request::StopAll
            } else {
                Request::Stop {
                    id: id.context("supply a task ID or --all")?,
                }
            }
        }
        Command::Shutdown => Request::Shutdown,
        Command::Configure {
            max_concurrent,
            max_per_project,
            timeout_secs,
            max_starts_per_day,
            provider,
            planning_enabled,
            planner_provider,
            planner_cooldown_secs,
            max_plans_per_day,
            auto_approve_read_only,
        } => {
            let mut settings = snapshot(&dir).await?.settings;
            if let Some(value) = max_concurrent {
                settings.max_concurrent = value;
            }
            if let Some(value) = max_per_project {
                settings.max_per_project = value;
            }
            if let Some(value) = timeout_secs {
                settings.task_timeout_secs = value;
            }
            if let Some(value) = max_starts_per_day {
                settings.max_starts_per_day = value;
            }
            if let Some(value) = provider {
                settings.preferred_provider = value.into();
            }
            if let Some(value) = planning_enabled {
                settings.planning_enabled = value;
            }
            if let Some(value) = planner_provider {
                settings.planner_provider = value.into();
            }
            if let Some(value) = planner_cooldown_secs {
                settings.planner_cooldown_secs = value;
            }
            if let Some(value) = max_plans_per_day {
                settings.max_plans_per_day = value;
            }
            if let Some(value) = auto_approve_read_only {
                settings.auto_approve_read_only = value;
            }
            Request::Configure { settings }
        }
        Command::Events { mut after, follow } => loop {
            let events: Vec<Event> =
                serde_json::from_value(send(&dir, Request::Events { after }).await?)?;
            for event in events {
                after = event.sequence;
                if cli.json {
                    println!("{}", serde_json::to_string(&event)?);
                } else {
                    println!(
                        "{} {} {} {}",
                        event.sequence,
                        event.task_id.as_deref().unwrap_or("service"),
                        event.kind,
                        event.message
                    );
                }
            }
            if !follow {
                return Ok(());
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            }
        },
        Command::Inspect { id } => {
            let state = snapshot(&dir).await?;
            let task = state
                .tasks
                .iter()
                .find(|t| t.id == id)
                .context("unknown task")?;
            println!("{}", serde_json::to_string_pretty(task)?);
            return Ok(());
        }
        Command::Open { id } => {
            let state = snapshot(&dir).await?;
            let task = state
                .tasks
                .iter()
                .find(|t| t.id == id)
                .context("unknown task")?;
            if task.status.occupies_slot() {
                bail!(
                    "worker is active or requires recovery; use events/inspect to observe without starting a competing session"
                );
            }
            let session = task
                .session_id
                .as_ref()
                .context("no persisted provider session")?;
            let cwd = task.worktree.as_ref().unwrap_or(&task.proposal.project);
            let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
            println!("cd {}", quote(&cwd.to_string_lossy()));
            match task.proposal.provider {
                Provider::Codex => {
                    println!("codex resume --include-non-interactive {}", quote(session))
                }
                Provider::Claude => println!("claude --resume {}", quote(session)),
                Provider::Mock => println!("Mock tasks have no native session"),
            }
            return Ok(());
        }
        Command::ResolveRecovery {
            id,
            confirmed_stopped,
        } => {
            if !confirmed_stopped {
                bail!("confirm the old worker is stopped, then pass --confirmed-stopped");
            }
            Request::ResolveRecovery { id }
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&send(&dir, request).await?)?
    );
    Ok(())
}
