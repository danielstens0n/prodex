use crate::model::*;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

/// Owned by the service actor: all scheduling decisions and writes are serialized.
pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS projects (path TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT, kind TEXT NOT NULL, message TEXT NOT NULL, created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS launches (task_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS managed_sessions (id TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS planning_runs (id TEXT PRIMARY KEY, project TEXT NOT NULL, created_at INTEGER NOT NULL, finished INTEGER NOT NULL DEFAULT 0);")?;
        let mut store = Self { db };
        let version: Option<u32> = store.meta("schema_version")?;
        match version {
            None => store.set_meta("schema_version", &2u32)?,
            Some(1) => {
                // An old global/all-project switch is not folder-level consent.
                let mut settings = store.settings()?;
                let legacy_all = settings.project_scope.is_none();
                if legacy_all {
                    settings.project_scope = Some(vec![]);
                    settings.planning_enabled = false;
                    settings.paused = true;
                }
                let mut projects = store.projects()?;
                for project in &mut projects {
                    project.enabled = settings
                        .project_scope
                        .as_ref()
                        .is_some_and(|paths| paths.contains(&project.path));
                }
                store.save_activation(&settings, &projects)?;
                // Existing queued work has no trustworthy provenance. Require
                // observation for it rather than silently grandfathering it in.
                for mut task in store.tasks()? {
                    task.automatic = true;
                    store.save_task(&task)?;
                }
                store.event(None, "permissions_migrated", "Explicit project folders and live Codex observation are now required for automatic work")?;
                store.set_meta("schema_version", &2u32)?;
            }
            Some(2) | Some(3) => {}
            Some(other) => bail!("unsupported database schema {other}"),
        }
        if store.meta::<u32>("schema_version")? == Some(2) {
            // A launch is charged per attempt, not per task. Preserve historical
            // charges while allowing explicit retries of an existing task ID.
            let tx = store.db.transaction()?;
            tx.execute_batch(
                "ALTER TABLE launches RENAME TO launches_v2;
                CREATE TABLE launches (task_id TEXT NOT NULL, attempt INTEGER NOT NULL,
                    created_at INTEGER NOT NULL, PRIMARY KEY(task_id, attempt));
                INSERT INTO launches(task_id,attempt,created_at)
                    SELECT task_id,0,created_at FROM launches_v2;
                DROP TABLE launches_v2;
                UPDATE meta SET value='3' WHERE key='schema_version';",
            )?;
            tx.commit()?;
        }
        Ok(store)
    }

    fn meta<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = self
            .db
            .query_row("SELECT value FROM meta WHERE key=?", [key], |row| {
                row.get(0)
            })
            .optional()?;
        raw.map(|value| serde_json::from_str(&value).context("invalid stored metadata"))
            .transpose()
    }

    fn set_meta(&self, key: &str, value: &impl Serialize) -> Result<()> {
        self.db.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, serde_json::to_string(value)?])?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings> {
        Ok(self.meta("settings")?.unwrap_or_default())
    }
    pub fn record_managed_session(&self, id: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO managed_sessions(id) VALUES (?)",
            [id],
        )?;
        Ok(())
    }
    pub fn managed_sessions(&self) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare("SELECT id FROM managed_sessions")?;
        Ok(stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        settings.validate().map_err(anyhow::Error::msg)?;
        self.set_meta("settings", settings)
    }

    /// Persist activation and all project selection flags as one transaction.
    pub fn save_activation(&mut self, settings: &Settings, projects: &[Project]) -> Result<()> {
        settings.validate().map_err(anyhow::Error::msg)?;
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO meta(key,value) VALUES ('settings',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(settings)?])?;
        for project in projects {
            tx.execute(
                "INSERT INTO projects(data,path) VALUES (?,?) ON CONFLICT(path) DO UPDATE SET data=excluded.data",
                params![
                    serde_json::to_string(project)?,
                    project
                        .path
                        .to_str()
                        .context("project path must be UTF-8")?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.read_json("SELECT data FROM projects ORDER BY path")
    }
    pub fn tasks(&self) -> Result<Vec<TaskRecord>> {
        self.read_json("SELECT data FROM tasks ORDER BY rowid")
    }
    fn read_json<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn project(&self, path: &Path) -> Result<Project> {
        self.projects()?
            .into_iter()
            .find(|p| p.path == path)
            .context("project not configured; use project set first")
    }
    pub fn task(&self, id: &str) -> Result<TaskRecord> {
        let raw: String = self
            .db
            .query_row("SELECT data FROM tasks WHERE id=?", [id], |row| row.get(0))
            .context("unknown task")?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save_project(&self, project: &Project) -> Result<()> {
        self.db.execute("INSERT INTO projects(path,data) VALUES (?,?) ON CONFLICT(path) DO UPDATE SET data=excluded.data",
            params![project.path.to_str().context("project path must be UTF-8")?, serde_json::to_string(project)?])?;
        Ok(())
    }
    pub fn save_task(&self, task: &TaskRecord) -> Result<()> {
        self.db.execute("INSERT INTO tasks(id,data) VALUES (?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![task.id, serde_json::to_string(task)?])?;
        Ok(())
    }
    pub fn event(&self, task_id: Option<&str>, kind: &str, message: &str) -> Result<()> {
        // Bound individual events even when a provider is noisy.
        let message: String = message.chars().take(16_384).collect();
        self.db.execute(
            "INSERT INTO events(task_id,kind,message,created_at) VALUES (?,?,?,?)",
            params![task_id, kind, message, now() as i64],
        )?;
        Ok(())
    }
    pub fn events(&self, after: i64) -> Result<Vec<Event>> {
        let mut stmt = self.db.prepare("SELECT sequence,task_id,kind,message,created_at FROM events WHERE sequence>? ORDER BY sequence LIMIT 500")?;
        Ok(stmt
            .query_map([after], |row| {
                Ok(Event {
                    sequence: row.get(0)?,
                    task_id: row.get(1)?,
                    kind: row.get(2)?,
                    message: row.get(3)?,
                    created_at: row.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn starts_today(&self) -> Result<usize> {
        let start = now() / 86400 * 86400;
        Ok(self.db.query_row(
            "SELECT count(*) FROM launches WHERE created_at>=?",
            [start as i64],
            |row| row.get::<_, i64>(0),
        )? as usize)
    }

    pub fn plan_allowed(&self, project: &Path, settings: &Settings) -> Result<bool> {
        Ok(self
            .next_plan_at(project, settings)?
            .is_none_or(|time| time <= now()))
    }

    pub fn planning_passes_today(&self) -> Result<usize> {
        let start = now() / 86400 * 86400;
        Ok(self.db.query_row(
            "SELECT count(*) FROM planning_runs WHERE created_at>=?",
            [start as i64],
            |row| row.get::<_, i64>(0),
        )? as usize)
    }

    pub fn next_plan_at(&self, project: &Path, settings: &Settings) -> Result<Option<u64>> {
        let start = now() / 86400 * 86400;
        let count = self.planning_passes_today()?;
        let latest: Option<i64> = self.db.query_row(
            "SELECT max(created_at) FROM planning_runs WHERE project=?",
            [project.to_string_lossy().as_ref()],
            |row| row.get(0),
        )?;
        let cooldown =
            latest.map(|time| (time.max(0) as u64).saturating_add(settings.planner_cooldown_secs));
        Ok(if count >= settings.max_plans_per_day {
            Some(cooldown.unwrap_or(0).max(start + 86400))
        } else {
            cooldown
        })
    }

    pub fn save_plan_result(&self, project: &Path, message: &str) -> Result<()> {
        self.set_meta(
            &format!("plan_result:{}", project.display()),
            &message.chars().take(512).collect::<String>(),
        )
    }

    pub fn plan_result(&self, project: &Path) -> Result<Option<String>> {
        self.meta(&format!("plan_result:{}", project.display()))
    }

    pub fn project_notes(&self, project: &Path) -> Result<Option<crate::planner::ProjectNotes>> {
        self.meta(&format!("project_notes:{}", project.display()))
    }

    pub fn save_project_notes(&self, project: &Project, text: &str) -> Result<()> {
        if text.trim().is_empty()
            || text.len() > crate::planner::MAX_NOTES_BYTES
            || text.contains('\0')
        {
            bail!("invalid project notes");
        }
        self.set_meta(
            &format!("project_notes:{}", project.path.display()),
            &crate::planner::ProjectNotes {
                objective_version: project.objective_version,
                text: text.into(),
            },
        )
    }

    pub fn record_plan(&self, id: &str, project: &Path) -> Result<()> {
        self.db.execute(
            "INSERT INTO planning_runs(id,project,created_at) VALUES (?,?,?)",
            params![id, project.to_string_lossy().as_ref(), now() as i64],
        )?;
        Ok(())
    }

    pub fn finish_plan(&self, id: &str) -> Result<()> {
        self.db
            .execute("UPDATE planning_runs SET finished=1 WHERE id=?", [id])?;
        Ok(())
    }

    /// Persist reservation and budget charge together before creating external processes.
    pub fn save_merge_baseline(
        &self,
        id: &str,
        value: &crate::integration::Baseline,
    ) -> Result<()> {
        self.set_meta(&format!("merge_baseline:{id}"), value)
    }
    pub fn merge_baseline(&self, id: &str) -> Result<crate::integration::Baseline> {
        self.meta(&format!("merge_baseline:{id}"))?
            .context("Merge baseline missing")
    }
    pub fn complete_merge(
        &mut self,
        job: &mut TaskRecord,
        source: &mut TaskRecord,
        summary: String,
    ) -> Result<()> {
        job.status = TaskStatus::Succeeded;
        job.pid = None;
        job.summary = summary;
        job.updated_at = now();
        source.review = ReviewStatus::Integrated;
        source.updated_at = now();
        let tx = self.db.transaction()?;
        for task in [job, source] {
            tx.execute(
                "UPDATE tasks SET data=? WHERE id=?",
                params![serde_json::to_string(task)?, task.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn reserve(&mut self, task: &mut TaskRecord) -> Result<()> {
        task.status = TaskStatus::Starting;
        task.updated_at = now();
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE tasks SET data=? WHERE id=?",
            params![serde_json::to_string(task)?, task.id],
        )?;
        tx.execute(
            "INSERT INTO launches(task_id,attempt,created_at) VALUES (?,?,?)",
            params![task.id, task.attempts.len() as i64, now() as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn recover(&self) -> Result<()> {
        let mut changed = false;
        let unfinished: i64 = self.db.query_row(
            "SELECT count(*) FROM planning_runs WHERE finished=0",
            [],
            |row| row.get(0),
        )?;
        if unfinished > 0 {
            changed = true;
            self.event(None, "planner_recovery_required", "Daemon stopped during planning. Verify old provider processes have stopped before resuming; no proposals were automatically recovered.")?;
            self.db
                .execute("UPDATE planning_runs SET finished=1 WHERE finished=0", [])?;
        }
        for mut task in self.tasks()? {
            if task.status == TaskStatus::Failed {
                task.status = TaskStatus::NeedsRetry;
                self.save_task(&task)?;
                self.event(
                    Some(&task.id),
                    "retry_available",
                    "Previous failed attempt is available for explicit retry",
                )?;
            }
            if matches!(
                task.status,
                TaskStatus::Starting | TaskStatus::Running | TaskStatus::Stopping
            ) {
                task.status = TaskStatus::RecoveryRequired;
                task.summary = "Daemon restarted during execution. Confirm the old worker is stopped before resolving recovery; no automatic retry.".into();
                task.updated_at = now();
                self.save_task(&task)?;
                self.event(Some(&task.id), "recovery_required", &task.summary)?;
                changed = true;
            }
        }
        if changed {
            let mut settings = self.settings()?;
            settings.paused = true;
            self.save_settings(&settings)?;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            protocol_version: PROTOCOL_VERSION,
            settings: self.settings()?,
            projects: self.projects()?,
            tasks: self.tasks()?,
            observation: Observation::default(),
            planning_activity: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_launch_history_without_losing_budget_charges() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("legacy.db");
        let db = Connection::open(&path)?;
        db.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            INSERT INTO meta VALUES('schema_version','2');
            CREATE TABLE launches(task_id TEXT PRIMARY KEY,created_at INTEGER NOT NULL);",
        )?;
        db.execute("INSERT INTO launches VALUES('old-task',?)", [now() as i64])?;
        drop(db);
        let store = Store::open(&path)?;
        assert_eq!(store.meta::<u32>("schema_version")?, Some(3));
        assert_eq!(store.starts_today()?, 1);
        store.db.execute(
            "INSERT INTO launches VALUES('old-task',1,?)",
            [now() as i64],
        )?;
        assert_eq!(store.starts_today()?, 2);
        assert!(
            store
                .db
                .execute(
                    "INSERT INTO launches VALUES('old-task',1,?)",
                    [now() as i64]
                )
                .is_err()
        );
        drop(store);
        assert_eq!(Store::open(&path)?.starts_today()?, 2);
        Ok(())
    }

    #[test]
    fn restart_preserves_identity_and_blocks_unverified_workers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let store = Store::open(&path)?;
        let mut task = TaskRecord {
            attempts: Vec::new(),
            automatic: false,
            started_at: None,
            id: "test".into(),
            proposal: TaskProposal {
                brief: None,
                risk: RiskLevel::Medium,
                project: dir.path().into(),
                objective_version: 1,
                prompt: "check".into(),
                rationale: "test".into(),
                provider: Provider::Mock,
                mode: TaskMode::ReadOnly,
                dependencies: vec![],
                expected_files: vec![],
                completion_criteria: "done".into(),
            },
            status: TaskStatus::Running,
            review: ReviewStatus::NotRequired,
            session_id: Some("session".into()),
            worktree: None,
            branch: None,
            base_commit: None,
            pid: Some(42),
            summary: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        store.save_task(&task)?;
        drop(store);
        let store = Store::open(&path)?;
        store.recover()?;
        task = store.task("test")?;
        assert_eq!(task.status, TaskStatus::RecoveryRequired);
        assert_eq!(task.session_id.as_deref(), Some("session"));
        assert!(store.settings()?.paused);
        assert!(task.status.occupies_slot());
        task.status = TaskStatus::Failed;
        task.pid = None;
        task.summary = "Workspace preparation failed".into();
        store.save_task(&task)?;
        store.save_settings(&Settings::default())?;
        store.recover()?;
        let retry = store.task("test")?;
        assert_eq!(retry.status, TaskStatus::NeedsRetry);
        assert_eq!(retry.summary, task.summary);
        assert_eq!(retry.session_id, task.session_id);
        assert!(!store.settings()?.paused);
        Ok(())
    }
    #[test]
    fn activation_transaction_rolls_back_settings_when_project_update_fails() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("state.db"))?;
        let project = Project {
            use_worktrees: true,
            path: directory.path().into(),
            objective: "Existing objective".into(),
            objective_version: 1,
            enabled: false,
        };
        store.save_project(&project)?;
        let before = serde_json::to_value(store.settings()?)?;
        store.db.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE ON projects BEGIN SELECT RAISE(ABORT, 'test failure'); END;")?;
        let settings = Settings {
            paused: true,
            project_scope: Some(vec![]),
            ..Settings::default()
        };
        assert!(
            store
                .save_activation(
                    &settings,
                    &[Project {
                        enabled: true,
                        ..project.clone()
                    }]
                )
                .is_err()
        );
        assert_eq!(before, serde_json::to_value(store.settings()?)?);
        assert!(!store.project(&project.path)?.enabled);
        Ok(())
    }

    #[test]
    fn old_settings_default_to_no_project_permission() {
        let settings: Settings =
            serde_json::from_str(r#"{"paused":true,"planning_enabled":false}"#).unwrap();
        assert_eq!(settings.project_scope, Some(vec![]));
        assert!(settings.paused);
    }

    #[test]
    fn legacy_all_projects_never_migrates_to_folder_consent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let store = Store::open(&path)?;
        store.set_meta("schema_version", &1u32)?;
        store.set_meta(
            "settings",
            &serde_json::json!({"project_scope":null,"planning_enabled":true,"paused":false}),
        )?;
        store.save_project(&Project {
            use_worktrees: true,
            path: dir.path().into(),
            objective: "Keep this objective".into(),
            objective_version: 1,
            enabled: true,
        })?;
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(store.settings()?.project_scope, Some(vec![]));
        assert!(!store.settings()?.planning_enabled);
        assert!(store.settings()?.paused);
        assert!(!store.projects()?[0].enabled);
        assert_eq!(store.projects()?[0].objective, "Keep this objective");
        assert_eq!(store.meta::<u32>("schema_version")?, Some(3));
        Ok(())
    }

    #[test]
    fn older_projects_default_to_worktrees_and_main_folder_choice_persists() -> Result<()> {
        let mut project: Project = serde_json::from_value(
            serde_json::json!({"path":"/project","objective":"Build app","objective_version":1,"enabled":true}),
        )?;
        assert!(project.use_worktrees);
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("state.db");
        let store = Store::open(&db)?;
        project.use_worktrees = false;
        store.save_project(&project)?;
        drop(store);
        assert!(!Store::open(&db)?.project(&project.path)?.use_worktrees);
        Ok(())
    }

    #[test]
    fn notebooks_survive_restart_remain_project_scoped_and_track_objective_version() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("notes.db");
        let project = Project {
            use_worktrees: true,
            path: dir.path().join("one"),
            objective: "Build checkout".into(),
            objective_version: 2,
            enabled: true,
        };
        let store = Store::open(&db)?;
        store.save_project_notes(
            &project,
            "Checkout incomplete; theme change rejected (reason unknown).",
        )?;
        drop(store);
        let store = Store::open(&db)?;
        let notes = store.project_notes(&project.path)?.unwrap();
        assert_eq!(notes.objective_version, 2);
        assert!(notes.text.contains("reason unknown"));
        assert!(store.project_notes(&dir.path().join("two"))?.is_none());
        assert!(store.save_project_notes(&project, " ").is_err());
        assert_eq!(store.project_notes(&project.path)?.unwrap(), notes);
        store.save_project_notes(
            &Project {
                objective_version: 3,
                ..project.clone()
            },
            "Checkout now works; verify integration.",
        )?;
        assert_eq!(
            store
                .project_notes(&project.path)?
                .unwrap()
                .objective_version,
            3
        );
        Ok(())
    }

    #[test]
    fn migration_preserves_explicit_folder_selection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let store = Store::open(&path)?;
        store.set_meta("schema_version", &1u32)?;
        store.save_settings(&Settings {
            project_scope: Some(vec![dir.path().into()]),
            planning_enabled: true,
            ..Settings::default()
        })?;
        store.save_project(&Project {
            use_worktrees: true,
            path: dir.path().into(),
            objective: "Explicit objective".into(),
            objective_version: 1,
            enabled: true,
        })?;
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(
            store.settings()?.project_scope,
            Some(vec![dir.path().into()])
        );
        assert!(store.settings()?.planning_enabled && store.projects()?[0].enabled);
        assert!(!store.snapshot()?.observation.available);
        Ok(())
    }
}
