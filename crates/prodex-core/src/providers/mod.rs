pub mod claude;
pub mod codex;

use crate::model::{Provider, TaskMode};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub provider: Provider,
    pub cwd: PathBuf,
    pub prompt: String,
    pub mode: TaskMode,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderOutput {
    Session(String),
    Text(String),
    Completed { success: bool, summary: String },
    Usage { usd: f64 },
    Error(String),
}

pub fn command(spec: &RunSpec) -> anyhow::Result<tokio::process::Command> {
    match spec.provider {
        Provider::Codex => codex::command(spec),
        Provider::Claude => claude::command(spec),
        Provider::Mock => anyhow::bail!("mock execution is handled by the service"),
    }
}

pub fn parse_line(provider: Provider, line: &str) -> Vec<ProviderOutput> {
    match provider {
        Provider::Codex => codex::parse_line(line),
        Provider::Claude => claude::parse_line(line),
        Provider::Mock => vec![],
    }
}
