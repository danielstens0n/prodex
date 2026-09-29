//! Pure launch checks. The service must apply these while holding its scheduling
//! lock and reserve the slot before checking another candidate. File scopes are
//! scheduling hints, never a filesystem sandbox.

use crate::model::*;
use std::collections::HashSet;

/// Automatic work needs current evidence; persisted history never grants activity.
pub fn automatic_eligible(project: &std::path::Path, snapshot: &Snapshot) -> Result<(), String> {
    if snapshot.settings.paused || !snapshot.settings.planning_enabled {
        return Err("automatic work is off or paused".into());
    }
    if !snapshot
        .settings
        .project_scope
        .as_ref()
        .is_some_and(|paths| paths.iter().any(|p| p == project))
        || !snapshot
            .projects
            .iter()
            .any(|p| p.path == project && p.enabled)
    {
        return Err("project folder is not allowed".into());
    }
    let observation = &snapshot.observation;
    let fresh = |time: u64| time <= now() && now().saturating_sub(time) <= OBSERVATION_TTL_SECS;
    if !observation.available
        || !observation.checked_at.is_some_and(fresh)
        || !observation
            .sessions
            .iter()
            .any(|session| session.project == project && fresh(session.last_seen))
    {
        return Err("waiting for a live Codex session in this folder".into());
    }
    Ok(())
}

fn normalized_prompt(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn coding_task(proposal: &TaskProposal) -> Result<(), String> {
    // Mock workers exercise the lifecycle without launching an agent.
    if proposal.provider != Provider::Mock
        && !matches!(
            proposal.mode,
            TaskMode::Edit
                | TaskMode::EditInPlace
                | TaskMode::InitializeRepository
                | TaskMode::Merge
        )
    {
        return Err(
            "Prodex tasks must write code; read-only worker tasks are no longer supported".into(),
        );
    }
    Ok(())
}

fn proposal_fields(proposal: &TaskProposal, snapshot: &Snapshot) -> Result<(), String> {
    coding_task(proposal)?;
    if let Some(brief) = &proposal.brief {
        brief.validate()?;
    }
    for (label, value, maximum) in [
        ("prompt", proposal.prompt.as_str(), 32_768),
        ("rationale", proposal.rationale.as_str(), 8_192),
        (
            "completion criteria",
            proposal.completion_criteria.as_str(),
            8_192,
        ),
    ] {
        if value.trim().chars().count() < 3 || value.len() > maximum || value.contains('\0') {
            return Err(format!(
                "{label} must contain meaningful text and be at most {maximum} bytes"
            ));
        }
    }
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.path == proposal.project)
        .ok_or("project is not configured")?;
    if !project.enabled {
        return Err("project is disabled".into());
    }
    if project.objective.trim().is_empty() {
        return Err("project has no approved objective".into());
    }
    if project.objective_version != proposal.objective_version {
        return Err("proposal objective version is stale".into());
    }
    if proposal.dependencies.len() > 128 || proposal.expected_files.len() > 1_024 {
        return Err("too many dependencies or expected file paths".into());
    }
    let mut dependencies = HashSet::new();
    for id in &proposal.dependencies {
        if !dependencies.insert(id) {
            return Err("duplicate dependency".into());
        }
        let dependency = snapshot
            .tasks
            .iter()
            .find(|task| task.id == *id)
            .ok_or("dependency does not exist")?;
        if dependency.proposal.project != proposal.project {
            return Err("dependency belongs to a different project".into());
        }
    }
    for path in &proposal.expected_files {
        // Reject alternate platform separators, absolute/drive paths, traversal,
        // ambiguous wildcard scopes, and control characters on every platform.
        if path.is_empty()
            || path.len() > 4_096
            || path
                .chars()
                .any(|c| c.is_control() || "\\:*?[]".contains(c))
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(
                "expected files must be relative paths without traversal or wildcards".into(),
            );
        }
    }
    Ok(())
}

pub fn validate_proposal(proposal: &TaskProposal, snapshot: &Snapshot) -> Result<(), String> {
    proposal_fields(proposal, snapshot)?;
    let prompt = normalized_prompt(&proposal.prompt);
    if snapshot.tasks.iter().any(|task| {
        task.proposal.project == proposal.project
            && task.proposal.objective_version == proposal.objective_version
            && normalized_prompt(&task.proposal.prompt) == prompt
    }) {
        return Err("this task was already proposed for the current objective".into());
    }
    Ok(())
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    left.iter().any(|a| {
        right.iter().any(|b| {
            // Conservative on case-sensitive hosts too, to avoid races when moving
            // the same project to the default case-insensitive macOS filesystem.
            let a = a.to_lowercase();
            let b = b.to_lowercase();
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        })
    })
}

pub fn eligible(task: &TaskRecord, snapshot: &Snapshot, starts_today: usize) -> Result<(), String> {
    snapshot.settings.validate()?;
    if snapshot.settings.paused {
        return Err("scheduling is paused".into());
    }
    if task.status != TaskStatus::Queued {
        return Err("task is not queued".into());
    }
    proposal_fields(&task.proposal, snapshot)?;
    if task.automatic {
        automatic_eligible(&task.proposal.project, snapshot)?;
    }
    if starts_today >= snapshot.settings.max_starts_per_day {
        return Err("daily task-start budget is exhausted".into());
    }
    let active: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| task.status.occupies_slot())
        .collect();
    if active.len() >= snapshot.settings.max_concurrent {
        return Err("global concurrency limit reached".into());
    }
    if active
        .iter()
        .filter(|other| other.proposal.project == task.proposal.project)
        .count()
        >= snapshot.settings.max_per_project
    {
        return Err("project concurrency limit reached".into());
    }
    for id in &task.proposal.dependencies {
        if id == &task.id {
            return Err("task cannot depend on itself".into());
        }
        let dependency = snapshot
            .tasks
            .iter()
            .find(|other| &other.id == id)
            .ok_or("dependency does not exist")?;
        if task.proposal.mode == TaskMode::Merge {
            if dependency.status != TaskStatus::Succeeded
                || dependency.proposal.mode != TaskMode::Edit
                || dependency.proposal.project != task.proposal.project
                || dependency.worktree.is_none()
            {
                return Err("Merge requires a completed coding worktree in this project".into());
            }
            continue;
        }
        if dependency.status != TaskStatus::Succeeded
            || !matches!(
                dependency.review,
                ReviewStatus::NotRequired | ReviewStatus::Integrated
            )
        {
            return Err("dependency has not succeeded and completed required integration".into());
        }
    }
    let direct = matches!(task.proposal.mode, TaskMode::EditInPlace | TaskMode::Merge)
        || snapshot
            .projects
            .iter()
            .any(|p| p.path == task.proposal.project && !p.use_worktrees);
    if active.iter().any(|other| {
        other.proposal.project == task.proposal.project
            && (direct || matches!(other.proposal.mode, TaskMode::EditInPlace | TaskMode::Merge))
    }) {
        return Err("Main-folder editing runs one Prodex task at a time".into());
    }
    if active.iter().any(|other| {
        other.proposal.project == task.proposal.project
            && (task.proposal.mode == TaskMode::InitializeRepository
                || other.proposal.mode == TaskMode::InitializeRepository)
    }) {
        return Err("Git setup runs alone in the project".into());
    }
    if task.proposal.mode == TaskMode::Edit
        && active.iter().any(|other| {
            other.proposal.project == task.proposal.project
                && other.proposal.mode == TaskMode::Edit
                && scopes_overlap(
                    &task.proposal.expected_files,
                    &other.proposal.expected_files,
                )
        })
    {
        return Err("edit scope overlaps an active task".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> TaskRecord {
        TaskRecord {
            attempts: Vec::new(),
            automatic: false,
            started_at: None,
            id: id.into(),
            proposal: TaskProposal {
                brief: None,
                risk: RiskLevel::Medium,
                project: "/tmp/project".into(),
                objective_version: 1,
                prompt: format!("Implement {id}"),
                rationale: "Independent useful work".into(),
                provider: Provider::Mock,
                mode: TaskMode::ReadOnly,
                dependencies: vec![],
                expected_files: vec![],
                completion_criteria: "Relevant checks pass".into(),
            },
            status: TaskStatus::Queued,
            review: ReviewStatus::NotRequired,
            session_id: None,
            worktree: None,
            branch: None,
            base_commit: None,
            pid: None,
            summary: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            observation: Observation::default(),
            planning_activity: vec![],
            protocol_version: PROTOCOL_VERSION,
            settings: Settings::default(),
            projects: vec![Project {
                use_worktrees: true,
                path: "/tmp/project".into(),
                objective: "Build the project".into(),
                objective_version: 1,
                enabled: true,
            }],
            tasks: vec![],
        }
    }

    #[test]
    fn git_setup_exclusively_locks_the_project_in_both_directions() {
        for setup_active in [false, true] {
            let mut state = snapshot();
            state.settings.max_per_project = 3;
            let mut candidate = task("candidate");
            candidate.proposal.mode = if setup_active {
                TaskMode::Edit
            } else {
                TaskMode::InitializeRepository
            };
            candidate.proposal.expected_files = vec!["a.rs".into()];
            let mut other = task("active");
            other.status = TaskStatus::Running;
            other.proposal.mode = if setup_active {
                TaskMode::InitializeRepository
            } else {
                TaskMode::Edit
            };
            other.proposal.expected_files = vec!["b.rs".into()];
            state.tasks.push(other);
            assert!(
                eligible(&candidate, &state, 0)
                    .unwrap_err()
                    .contains("Git setup runs alone")
            );
        }
    }

    #[test]
    fn real_workers_reject_read_only_at_submission_and_launch() {
        let state = snapshot();
        for provider in [Provider::Codex, Provider::Claude] {
            let mut candidate = task("coding-only");
            candidate.proposal.provider = provider;
            assert!(
                validate_proposal(&candidate.proposal, &state)
                    .unwrap_err()
                    .contains("must write code")
            );
            assert!(
                eligible(&candidate, &state, 0)
                    .unwrap_err()
                    .contains("must write code")
            );
            candidate.proposal.mode = TaskMode::Edit;
            candidate.proposal.expected_files = vec!["src/parser.rs".into()];
            assert!(validate_proposal(&candidate.proposal, &state).is_ok());
            assert!(eligible(&candidate, &state, 0).is_ok());
        }
    }

    #[test]
    fn automatic_tasks_require_allowed_folder_and_fresh_observation_even_after_approval() {
        let mut state = snapshot();
        let mut candidate = task("automatic");
        candidate.automatic = true;
        state.settings.planning_enabled = true;
        state.settings.project_scope = Some(vec![candidate.proposal.project.clone()]);
        assert!(eligible(&candidate, &state, 0).is_err());
        state.observation = Observation {
            checked_at: Some(now()),
            available: true,
            detail: String::new(),
            sessions: vec![ObservedSession {
                id: "independent".into(),
                cwd: candidate.proposal.project.clone(),
                project: candidate.proposal.project.clone(),
                last_seen: now(),
                source: "fixture".into(),
            }],
        };
        assert!(eligible(&candidate, &state, 0).is_ok());
        for change in [
            "off",
            "pause",
            "unselected",
            "disabled",
            "unavailable",
            "stale_scan",
            "stale_session",
            "other_folder",
            "future",
        ] {
            let mut changed = state.clone();
            match change {
                "off" => changed.settings.planning_enabled = false,
                "pause" => changed.settings.paused = true,
                "unselected" => changed.settings.project_scope = Some(vec![]),
                "disabled" => changed.projects[0].enabled = false,
                "unavailable" => changed.observation.available = false,
                "stale_scan" => {
                    changed.observation.checked_at = Some(now() - OBSERVATION_TTL_SECS - 1)
                }
                "stale_session" => {
                    changed.observation.sessions[0].last_seen = now() - OBSERVATION_TTL_SECS - 1
                }
                "other_folder" => changed.observation.sessions[0].project = "/tmp/other".into(),
                "future" => changed.observation.checked_at = Some(now() + 100),
                _ => unreachable!(),
            }
            assert!(eligible(&candidate, &changed, 0).is_err(), "{change}");
        }
        // An explicit CLI submission remains possible without an observed session.
        candidate.automatic = false;
        state.observation = Observation::default();
        assert!(eligible(&candidate, &state, 0).is_ok());
    }

    #[test]
    fn three_slots_replenish_only_two_after_two_completions() {
        let mut state = snapshot();
        for id in ["one", "two", "three"] {
            let mut candidate = task(id);
            assert!(eligible(&candidate, &state, 0).is_ok());
            candidate.status = TaskStatus::Running;
            state.tasks.push(candidate);
        }
        assert!(eligible(&task("four"), &state, 0).is_err());
        state.tasks[0].status = TaskStatus::Succeeded;
        state.tasks[1].status = TaskStatus::Succeeded;
        for id in ["four", "five"] {
            let mut candidate = task(id);
            assert!(eligible(&candidate, &state, 0).is_ok());
            candidate.status = TaskStatus::Starting;
            state.tasks.push(candidate);
        }
        assert!(eligible(&task("six"), &state, 0).is_err());
    }

    #[test]
    fn pause_stale_objective_and_budget_rechecked_before_launch() {
        let candidate = task("one");
        let mut state = snapshot();
        assert!(validate_proposal(&candidate.proposal, &state).is_ok());
        state.settings.paused = true;
        assert!(eligible(&candidate, &state, 0).is_err());
        state.settings.paused = false;
        state.projects[0].objective_version += 1;
        assert!(eligible(&candidate, &state, 0).is_err());
        state.projects[0].objective_version = 1;
        assert!(eligible(&candidate, &state, state.settings.max_starts_per_day).is_err());
        state.projects[0].enabled = false;
        assert!(eligible(&candidate, &state, 0).is_err());
    }

    #[test]
    fn dependencies_need_integration_and_cannot_reference_self_or_unknown() {
        let mut state = snapshot();
        let mut dependency = task("dependency");
        dependency.status = TaskStatus::Succeeded;
        dependency.review = ReviewStatus::AwaitingReview;
        state.tasks.push(dependency);
        let mut candidate = task("candidate");
        candidate.proposal.dependencies.push("dependency".into());
        assert!(validate_proposal(&candidate.proposal, &state).is_ok());
        assert!(eligible(&candidate, &state, 0).is_err());
        state.tasks[0].review = ReviewStatus::Accepted;
        assert!(eligible(&candidate, &state, 0).is_err());
        state.tasks[0].review = ReviewStatus::Integrated;
        assert!(eligible(&candidate, &state, 0).is_ok());
        candidate.proposal.dependencies = vec!["candidate".into()];
        state.tasks.push(candidate.clone());
        assert!(eligible(&candidate, &state, 0).is_err());
        candidate.proposal.dependencies = vec!["missing".into()];
        assert!(validate_proposal(&candidate.proposal, &state).is_err());
    }

    #[test]
    fn unknown_scope_blocks_edits_and_recovery_occupies_capacity() {
        let mut state = snapshot();
        let mut active = task("active");
        active.status = TaskStatus::RecoveryRequired;
        active.proposal.mode = TaskMode::Edit;
        state.tasks.push(active);
        let mut candidate = task("candidate");
        candidate.proposal.mode = TaskMode::Edit;
        candidate.proposal.expected_files = vec!["src/new.rs".into()];
        assert!(eligible(&candidate, &state, 0).is_err());
        state.tasks[0].proposal.expected_files = vec!["docs".into()];
        assert!(eligible(&candidate, &state, 0).is_ok());
        state.tasks[0].proposal.expected_files = vec!["SRC".into()];
        assert!(eligible(&candidate, &state, 0).is_err());
        candidate.proposal.mode = TaskMode::ReadOnly;
        state.settings.max_per_project = 1;
        assert!(eligible(&candidate, &state, 0).is_err());
    }

    #[test]
    fn rejected_work_is_deduplicated_and_paths_are_lexically_checked() {
        let mut state = snapshot();
        let mut rejected = task("prior");
        rejected.status = TaskStatus::Rejected;
        state.tasks.push(rejected);
        let mut candidate = task("new");
        candidate.proposal.prompt = " IMPLEMENT   prior \n".into();
        assert!(validate_proposal(&candidate.proposal, &state).is_err());
        candidate.proposal.prompt = "Different useful task".into();
        for path in [
            "../escape",
            "/absolute",
            "src/../escape",
            "C:\\outside",
            "./file",
            "src//file",
            "src/*",
        ] {
            candidate.proposal.expected_files = vec![path.into()];
            assert!(
                validate_proposal(&candidate.proposal, &state).is_err(),
                "{path}"
            );
        }
        candidate.proposal.expected_files = vec!["src/file.rs".into()];
        assert!(validate_proposal(&candidate.proposal, &state).is_ok());
    }
}
