use crate::model::Provider;
use crate::providers::{self, ProviderOutput, RunSpec};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};

// Provider tool results can contain multiple copies of command output. Keep
// transport limits separate from summaries and the 1 MiB local control protocol.
const MAX_FRAME: usize = 16 * 1024 * 1024;
const MAX_SUMMARY: usize = 1024 * 1024;
const LARGE_FRAME: usize = 1024 * 1024;

#[derive(Debug)]
pub enum WorkerEvent {
    Diagnostic(String),
    Started {
        pid: Option<u32>,
    },
    Output(ProviderOutput),
    Finished {
        success: bool,
        interrupted: bool,
        summary: String,
    },
}

enum IoEvent {
    Line(String),
    Failure(String),
    Closed {
        stdout: bool,
        bytes: usize,
        frames: usize,
        largest: usize,
    },
}

async fn read_lines(reader: impl AsyncRead + Unpin, stdout: bool, tx: mpsc::Sender<IoEvent>) {
    let mut reader = BufReader::new(reader);
    let mut frame = Vec::new();
    let (mut bytes, mut frames, mut largest, mut current) = (0usize, 0usize, 0usize, 0usize);
    loop {
        let available = match reader.fill_buf().await {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = tx
                    .send(IoEvent::Failure(format!(
                        "provider {} stream: {error}",
                        if stdout { "stdout" } else { "stderr" }
                    )))
                    .await;
                return;
            }
        };
        if available.is_empty() {
            if current > 0 {
                frames += 1;
                largest = largest.max(current);
            }
            if stdout && !frame.is_empty() {
                match String::from_utf8(frame) {
                    Ok(line) => {
                        let _ = tx.send(IoEvent::Line(line)).await;
                    }
                    Err(_) => {
                        let _ = tx
                            .send(IoEvent::Failure("provider stdout is not UTF-8".into()))
                            .await;
                    }
                }
            }
            let _ = tx
                .send(IoEvent::Closed {
                    stdout,
                    bytes,
                    frames,
                    largest,
                })
                .await;
            return;
        }
        let length = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        bytes = bytes.saturating_add(length);
        current = current.saturating_add(length);
        let newline = available[length - 1] == b'\n';
        if newline {
            frames += 1;
            largest = largest.max(current);
            current = 0;
        }
        // Stderr is diagnostics, not JSONL. Drain it without allocating a line
        // buffer, including arbitrarily long lines and non-UTF-8 output.
        if !stdout {
            reader.consume(length);
            continue;
        }
        if frame.len() + length > MAX_FRAME {
            let _ = tx
                .send(IoEvent::Failure(
                    format!("Provider stdout JSON event exceeds {MAX_FRAME} bytes (16 MiB); observed at least {} bytes in this event, {bytes} stream bytes, {frames} frames. Output was not skipped; retry after reducing tool output.", frame.len() + length),
                ))
                .await;
            return;
        }
        frame.extend_from_slice(&available[..length]);
        reader.consume(length);
        if frame.last() == Some(&b'\n') {
            match String::from_utf8(std::mem::take(&mut frame)) {
                Ok(line) => {
                    if tx.send(IoEvent::Line(line)).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx
                        .send(IoEvent::Failure("provider stdout is not UTF-8".into()))
                        .await;
                    return;
                }
            }
        }
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid.filter(|p| *p > 1 && *p <= i32::MAX as u32) {
        // The child was spawned in its own process group; never use recovered PIDs.
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}

async fn terminate(child: &mut Child, pid: Option<u32>) {
    #[cfg(unix)]
    {
        signal_group(pid, libc::SIGTERM);
        // Give descendants the same grace period, even if the leader exits first.
        sleep(Duration::from_secs(2)).await;
        signal_group(pid, libc::SIGKILL);
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

// Keep the final agent message separate from progress commentary. An explicit
// provider completion summary takes precedence even over subsequent text events.
fn update_summary(summary: &mut String, explicit: &mut bool, output: &ProviderOutput) {
    let text = match output {
        ProviderOutput::Text(text) if !*explicit && !text.trim().is_empty() => text,
        ProviderOutput::Completed { summary: text, .. } if !text.trim().is_empty() => {
            *explicit = true;
            text
        }
        ProviderOutput::Error(text) => {
            *explicit = true;
            text
        }
        _ => return,
    };
    let mut length = text.len().min(MAX_SUMMARY);
    while !text.is_char_boundary(length) {
        length -= 1;
    }
    *summary = text[..length].to_owned();
}

pub async fn run(
    spec: RunSpec,
    timeout_secs: u64,
    mut cancel: oneshot::Receiver<()>,
    events: mpsc::Sender<WorkerEvent>,
) {
    if spec.provider == Provider::Mock {
        let _ = events.send(WorkerEvent::Started { pid: None }).await;
        let _ = events
            .send(WorkerEvent::Output(ProviderOutput::Session(format!(
                "mock-{}",
                uuid::Uuid::new_v4()
            ))))
            .await;
        let _ = events
            .send(WorkerEvent::Output(ProviderOutput::Text(
                "Mock worker running".into(),
            )))
            .await;
        let (success, interrupted, summary) = tokio::select! {
            _ = &mut cancel => (false, true, "Mock worker interrupted".to_owned()),
            _ = sleep(Duration::from_secs(timeout_secs)) => (false, false, "Worker timed out".to_owned()),
            _ = sleep(Duration::from_secs(3)) => (!spec.prompt.contains("mock:fail"), false, if spec.prompt.contains("mock:plan") { r#"{"proposals":[]}"#.to_owned() } else { "Mock worker finished".to_owned() }),
        };
        let _ = events
            .send(WorkerEvent::Finished {
                success,
                interrupted,
                summary,
            })
            .await;
        return;
    }
    let started = std::time::Instant::now();
    let _ = events.send(WorkerEvent::Diagnostic(format!(
        "Launching provider={:?} mode={:?} cwd={} timeout_secs={timeout_secs} resume={} stdout_frame_limit_bytes={MAX_FRAME}",
        spec.provider, spec.mode, spec.cwd.display(), spec.session_id.is_some()
    ))).await;
    let mut command = match providers::command(&spec) {
        Ok(command) => command,
        Err(error) => {
            finish(&events, false, false, error.to_string()).await;
            return;
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            finish(
                &events,
                false,
                false,
                format!("Cannot start provider: {error}"),
            )
            .await;
            return;
        }
    };
    let pid = child.id();
    let _ = events.send(WorkerEvent::Started { pid }).await;
    // Bound queued raw frames as well as individual frame size.
    let (tx, mut rx) = mpsc::channel(4);
    let out = tokio::spawn(read_lines(
        child.stdout.take().expect("piped stdout"),
        true,
        tx.clone(),
    ));
    let err = tokio::spawn(read_lines(
        child.stderr.take().expect("piped stderr"),
        false,
        tx.clone(),
    ));
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = tokio::spawn(async move {
        if let Err(error) = stdin.write_all(spec.prompt.as_bytes()).await {
            let _ = tx
                .send(IoEvent::Failure(format!(
                    "Cannot write provider prompt: {error}"
                )))
                .await;
        }
        // Dropping stdin supplies EOF after the prompt.
    });
    let deadline = sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(deadline);
    let mut status = None;
    let mut closed = 0;
    let mut completed = false;
    let mut failed = false;
    let mut summary = String::new();
    let mut explicit_summary = false;
    let mut reason = None;
    let mut interrupted = false;
    loop {
        if status.is_some() && closed == 2 {
            break;
        }
        tokio::select! {
            _ = &mut cancel => { interrupted = true; reason = Some("Worker interrupted".to_owned()); break; }
            _ = &mut deadline => { reason = Some("Worker timed out".to_owned()); break; }
            result = child.wait(), if status.is_none() && closed == 2 => match result {
                Ok(exit) => status = Some(exit),
                Err(error) => { reason = Some(format!("Cannot wait for provider: {error}")); break; }
            },
            event = rx.recv(), if closed < 2 => match event {
                Some(IoEvent::Closed { stdout, bytes, frames, largest }) => {
                    closed += 1;
                    let _ = events.send(WorkerEvent::Diagnostic(format!(
                        "{} closed: bytes={bytes} frames={frames} largest_frame_bytes={largest}",
                        if stdout { "stdout" } else { "stderr (contents omitted)" }
                    ))).await;
                },
                Some(IoEvent::Failure(message)) => { reason = Some(message); break; }
                None => { reason = Some("Provider streams ended unexpectedly".into()); break; }
                Some(IoEvent::Line(line)) => {
                    if line.len() >= LARGE_FRAME {
                        let _ = events.send(WorkerEvent::Diagnostic(format!(
                            "Large stdout JSON event: bytes={} (contents omitted)", line.len()
                        ))).await;
                    }
                    for output in providers::parse_line(spec.provider, &line) {
                        update_summary(&mut summary, &mut explicit_summary, &output);
                        match &output {
                            ProviderOutput::Completed { success, .. } => {
                                completed = true;
                                failed |= !success;
                            }
                            ProviderOutput::Error(_) => failed = true,
                            _ => {}
                        }
                        if events.send(WorkerEvent::Output(output)).await.is_err() {
                            reason = Some("Worker event consumer disconnected".into());
                            break;
                        }
                }},
            }
        }
        if reason.is_some() {
            break;
        }
    }
    if reason.is_some() {
        terminate(&mut child, pid).await;
    }
    input.abort();
    out.abort();
    err.abort();
    let _ = input.await;
    let _ = out.await;
    let _ = err.await;
    let success = reason.is_none() && completed && !failed && status.is_some_and(|s| s.success());
    let _ = events.send(WorkerEvent::Diagnostic(format!(
        "Provider finished: provider={:?} pid={pid:?} elapsed_ms={} exit={} completed_event={completed} provider_error={failed} interrupted={interrupted} success={success} stop_reason={}",
        spec.provider, started.elapsed().as_millis(),
        status.map_or_else(|| "unavailable (terminated or wait failed)".into(), |s| s.to_string()),
        reason.as_deref().unwrap_or("none")
    ))).await;
    if let Some(reason) = reason {
        summary = reason;
    } else if !success && summary.is_empty() {
        summary = "Provider exited without a successful completed turn".into();
    }
    finish(&events, success, interrupted, summary).await;
}

async fn finish(
    events: &mpsc::Sender<WorkerEvent>,
    success: bool,
    interrupted: bool,
    summary: String,
) {
    let _ = events
        .send(WorkerEvent::Finished {
            success,
            interrupted,
            summary,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskMode;

    #[test]
    fn final_text_replaces_commentary_and_explicit_completion_wins() {
        let mut summary = String::new();
        let mut explicit = false;
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Text("Researching now".into()),
        );
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Text(r#"{"proposals":[]}"#.into()),
        );
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Text("  ".into()),
        );
        assert_eq!(summary, r#"{"proposals":[]}"#);
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Completed {
                success: true,
                summary: String::new(),
            },
        );
        assert_eq!(summary, r#"{"proposals":[]}"#);
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Completed {
                success: true,
                summary: "Explicit result".into(),
            },
        );
        update_summary(
            &mut summary,
            &mut explicit,
            &ProviderOutput::Text("Late commentary".into()),
        );
        assert_eq!(summary, "Explicit result");
    }

    #[tokio::test]
    async fn mock_cancellation_is_terminal_and_preserves_identity() {
        let (tx, mut rx) = mpsc::channel(16);
        let (stop, cancel) = oneshot::channel();
        stop.send(()).unwrap();
        run(
            RunSpec {
                provider: Provider::Mock,
                cwd: "/tmp".into(),
                prompt: "test".into(),
                mode: TaskMode::ReadOnly,
                session_id: None,
            },
            30,
            cancel,
            tx,
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(WorkerEvent::Started { pid: None })
        ));
        assert!(matches!(
            rx.recv().await,
            Some(WorkerEvent::Output(ProviderOutput::Session(_)))
        ));
        assert!(matches!(
            rx.recv().await,
            Some(WorkerEvent::Output(ProviderOutput::Text(_)))
        ));
        assert!(matches!(
            rx.recv().await,
            Some(WorkerEvent::Finished {
                success: false,
                interrupted: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn overlong_frames_are_rejected_without_unbounded_buffering() {
        let (tx, mut rx) = mpsc::channel(4);
        let bytes = vec![b'x'; MAX_FRAME + 1];
        read_lines(bytes.as_slice(), true, tx).await;
        assert!(
            matches!(rx.recv().await, Some(IoEvent::Failure(message)) if message.contains("stdout") && message.contains("16 MiB"))
        );
    }

    #[tokio::test]
    async fn large_tool_events_preserve_following_completion_for_both_providers() {
        for (provider, tool, completion) in [
            (
                Provider::Codex,
                serde_json::json!({"type":"item.completed", "item":{
                    "type":"command_execution", "aggregated_output":"x".repeat(2_200_000)
                }}),
                r#"{"type":"turn.completed"}"#,
            ),
            (
                Provider::Claude,
                serde_json::json!({"type":"user", "message":{"content":[{
                    "type":"tool_result", "content":"x".repeat(2_200_000)
                }]}}),
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Done"}"#,
            ),
        ] {
            let input = format!("{tool}\n{completion}"); // Final event has no newline.
            let (tx, mut rx) = mpsc::channel(4);
            read_lines(input.as_bytes(), true, tx).await;
            let Some(IoEvent::Line(line)) = rx.recv().await else {
                panic!("lost tool event")
            };
            assert!(line.len() > LARGE_FRAME);
            assert!(providers::parse_line(provider, &line).is_empty());
            let Some(IoEvent::Line(line)) = rx.recv().await else {
                panic!("lost completion")
            };
            assert!(
                providers::parse_line(provider, &line)
                    .iter()
                    .any(|o| matches!(o, ProviderOutput::Completed { success: true, .. }))
            );
            assert!(
                matches!(rx.recv().await, Some(IoEvent::Closed { stdout: true, frames: 2, bytes, largest })
                if bytes == input.len() && largest > LARGE_FRAME)
            );
        }
    }

    #[tokio::test]
    async fn stderr_drains_large_non_utf8_output_without_frames_or_failure() {
        let (tx, mut rx) = mpsc::channel(4);
        let input = vec![0xff; MAX_FRAME + 1];
        read_lines(input.as_slice(), false, tx).await;
        assert!(
            matches!(rx.recv().await, Some(IoEvent::Closed { stdout: false, bytes, frames: 1, largest })
            if bytes == input.len() && largest == input.len())
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn invalid_stdout_remains_a_failure() {
        let (tx, mut rx) = mpsc::channel(4);
        read_lines(&[0xff, b'\n'][..], true, tx).await;
        assert!(matches!(rx.recv().await, Some(IoEvent::Failure(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn termination_reaps_the_owned_process_group() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & wait"])
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        terminate(&mut child, pid).await;
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn accepts_final_json_line_without_newline() {
        let (tx, mut rx) = mpsc::channel(4);
        read_lines(&b"{\"type\":\"turn.completed\"}"[..], true, tx).await;
        assert!(matches!(rx.recv().await, Some(IoEvent::Line(_))));
        assert!(matches!(rx.recv().await, Some(IoEvent::Closed { .. })));
    }
}
