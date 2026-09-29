use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// Version 8 adds managed background merge tasks and verified integration.
pub const PROTOCOL_VERSION: u32 = 8;
pub const OBSERVATION_TTL_SECS: u64 = 15;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Mock,
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    ReadOnly,
    Edit,
    EditInPlace,
    InitializeRepository,
    Merge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    AwaitingApproval,
    Queued,
    Starting,
    Running,
    Stopping,
    Succeeded,
    Failed,
    NeedsRetry,
    Interrupted,
    Rejected,
    /// A previous daemon died. User must confirm the old process is gone before retrying.
    RecoveryRequired,
}

impl TaskStatus {
    pub fn occupies_slot(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Stopping | Self::RecoveryRequired
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    NotRequired,
    AwaitingReview,
    Accepted,
    Rejected,
    Integrated,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub paused: bool,
    /// Explicit allowlist. Legacy None is migrated to an empty selection.
    pub project_scope: Option<Vec<PathBuf>>,
    pub max_concurrent: usize,
    pub max_per_project: usize,
    pub preferred_provider: Provider,
    pub task_timeout_secs: u64,
    /// Hard cap on explicitly approved tasks, not an inferred USD estimate.
    pub max_starts_per_day: usize,
    pub planning_enabled: bool,
    pub planning_free_slots: usize,
    pub max_proposal_risk: RiskLevel,
    pub planner_provider: Provider,
    pub planner_cooldown_secs: u64,
    pub max_plans_per_day: usize,
    pub auto_approve_read_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            paused: false,
            project_scope: Some(vec![]),
            max_concurrent: 3,
            max_per_project: 3,
            preferred_provider: Provider::Codex,
            task_timeout_secs: 600,
            max_starts_per_day: 20,
            planning_enabled: false,
            planning_free_slots: 2,
            max_proposal_risk: RiskLevel::Medium,
            planner_provider: Provider::Codex,
            planner_cooldown_secs: 60,
            max_plans_per_day: 1_440,
            auto_approve_read_only: false,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.project_scope.is_none() {
            return Err(
                "select explicit project folders; all-project activation is no longer supported"
                    .into(),
            );
        }
        if !(1..=32).contains(&self.max_concurrent) || !(1..=32).contains(&self.max_per_project) {
            return Err("concurrency must be between 1 and 32".into());
        }
        if !(1..=86400).contains(&self.task_timeout_secs) || self.max_starts_per_day == 0 {
            return Err(
                "timeout must be 1..86400 seconds and daily starts must be positive".into(),
            );
        }
        if !(1..=32).contains(&self.planning_free_slots) {
            return Err("planning free-slot threshold must be between 1 and 32".into());
        }
        if self.planner_cooldown_secs < 30 || self.max_plans_per_day == 0 {
            return Err(
                "planner cooldown must be at least 30 seconds and daily passes must be positive"
                    .into(),
            );
        }
        Ok(())
    }
}

fn default_worktrees() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "default_worktrees")]
    pub use_worktrees: bool,
    pub path: PathBuf,
    pub objective: String,
    pub objective_version: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBrief {
    pub title: String,
    pub change: String,
    pub approach: String,
}

impl TaskBrief {
    pub fn validate(&self) -> Result<(), String> {
        for (label, value, limit) in [
            ("title", &self.title, 100),
            ("change", &self.change, 400),
            ("approach", &self.approach, 700),
        ] {
            if value.trim().is_empty() || value.chars().count() > limit || value.contains('\0') {
                return Err(format!(
                    "task brief {label} must contain 1..{limit} characters"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProposal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<TaskBrief>,
    pub project: PathBuf,
    pub objective_version: u64,
    pub prompt: String,
    pub rationale: String,
    pub provider: Provider,
    pub mode: TaskMode,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub expected_files: Vec<String>,
    pub completion_criteria: String,
    #[serde(default)]
    pub risk: RiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAttempt {
    pub status: TaskStatus,
    pub session_id: Option<String>,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub summary: String,
    pub started_at: Option<u64>,
    pub finished_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    #[serde(default)]
    pub attempts: Vec<TaskAttempt>,
    pub id: String,
    /// Automatic planner output retains observation gating even after approval.
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub started_at: Option<u64>,
    pub proposal: TaskProposal,
    pub status: TaskStatus,
    pub review: ReviewStatus,
    pub session_id: Option<String>,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub pid: Option<u32>,
    pub summary: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub sequence: i64,
    pub task_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol_version: u32,
    pub settings: Settings,
    pub projects: Vec<Project>,
    pub tasks: Vec<TaskRecord>,
    #[serde(default)]
    pub observation: Observation,
    #[serde(default)]
    pub planning_activity: Vec<PlanningActivity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningActivity {
    pub project: PathBuf,
    pub message: String,
    pub next_check_at: Option<u64>,
    #[serde(default)]
    pub daily_limit_reached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedSession {
    pub id: String,
    pub cwd: PathBuf,
    pub project: PathBuf,
    pub last_seen: u64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub checked_at: Option<u64>,
    pub available: bool,
    pub detail: String,
    pub sessions: Vec<ObservedSession>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            checked_at: None,
            available: false,
            detail: "Waiting for the first Codex session observation".into(),
            sessions: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    SetActivation {
        enabled: bool,
        projects: Option<Vec<PathBuf>>,
    },
    Status,
    SetWorktrees {
        project: PathBuf,
        enabled: bool,
    },
    Merge {
        id: String,
    },
    ConfirmIntegrated {
        id: String,
    },
    MarkReviewed {
        id: String,
    },
    Configure {
        settings: Settings,
    },
    Project {
        path: PathBuf,
        objective: String,
        enabled: bool,
    },
    Submit {
        proposal: TaskProposal,
        approved: bool,
    },
    Retry {
        id: String,
    },
    Approve {
        id: String,
    },
    Reject {
        id: String,
    },
    Pause,
    Resume,
    Stop {
        id: String,
    },
    StopAll,
    CheckNow {
        project: PathBuf,
    },
    UnconnectProject {
        project: PathBuf,
    },
    Plan {
        project: PathBuf,
    },
    Events {
        after: i64,
    },
    /// An explicit acknowledgement; never kills a PID recovered from disk.
    ResolveRecovery {
        id: String,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub data: serde_json::Value,
    pub error: Option<String>,
}

impl Response {
    pub fn success(value: impl Serialize) -> Self {
        Self {
            ok: true,
            data: serde_json::to_value(value).unwrap_or_default(),
            error: None,
        }
    }
    pub fn failure(message: impl ToString) -> Self {
        Self {
            ok: false,
            data: serde_json::Value::Null,
            error: Some(message.to_string()),
        }
    }
}
