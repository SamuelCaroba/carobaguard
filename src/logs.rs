use std::{convert::Infallible, sync::Arc, time::Duration};

use crate::{
    AppState,
    auth::AuthUser,
    error::{ApiError, ApiResult},
};
use async_stream::stream;
use axum::{
    extract::{Query, State},
    response::{Sse, sse::Event},
};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_util::codec::{FramedRead, LinesCodec};

const MAX_LOG_LINE_BYTES: usize = 64 * 1024;
const MAX_DOCKER_FRAME_BYTES: usize = 1024 * 1024;
const MAX_DOCKER_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_DURATION: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Clone)]
pub struct LogService {
    slots: Arc<Semaphore>,
}

impl LogService {
    pub fn new(max_streams: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(max_streams)),
        }
    }

    fn acquire(&self) -> ApiResult<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_requests("log stream limit reached"))
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LogSource {
    Docker,
    Systemd,
}

#[derive(Deserialize)]
pub struct LogQuery {
    source: LogSource,
    target: String,
    tail: Option<usize>,
    since: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogMessage {
    Line { line: String },
    Status { message: String },
    Error { message: String },
}

impl LogMessage {
    fn event_name(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::Status { .. } => "status",
            Self::Error { .. } => "stream_error",
        }
    }
}

pub async fn events(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<LogQuery>,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    if query.target.is_empty() || query.target.len() > 256 {
        return Err(ApiError::bad_request("invalid log target"));
    }
    let permit = state.logs.acquire()?;
    let tail = query.tail.unwrap_or(300).clamp(1, 2000);
    let mut receiver = match query.source {
        LogSource::Docker => {
            docker_stream(
                state.docker.clone(),
                &query.target,
                tail,
                query.since.unwrap_or(0),
            )
            .await?
        }
        LogSource::Systemd => systemd_stream(state.systemd.clone(), &query.target, tail)?,
    };

    let events = stream! {
        let _permit = permit;
        let maximum_duration = tokio::time::sleep(MAX_STREAM_DURATION);
        tokio::pin!(maximum_duration);
        loop {
            tokio::select! {
                message = receiver.recv() => {
                    let Some(message) = message else { break; };
                    if let Ok(data) = serde_json::to_string(&message) {
                        yield Ok(Event::default().event(message.event_name()).data(data));
                    }
                }
                _ = &mut maximum_duration => {
                    let message = LogMessage::Status { message: "maximum stream duration reached".to_owned() };
                    if let Ok(data) = serde_json::to_string(&message) {
                        yield Ok(Event::default().event("status").data(data));
                    }
                    break;
                }
            }
        }
    };
    Ok(Sse::new(events).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn systemd_stream(
    service: crate::services::SystemdService,
    unit: &str,
    tail: usize,
) -> ApiResult<mpsc::Receiver<LogMessage>> {
    let mut command = service
        .follow_logs_command(unit, tail)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let mut child = command
        .spawn()
        .map_err(|error| ApiError::service_unavailable(error.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::internal("journal stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ApiError::internal("journal stderr was not piped"))?;
    let (sender, receiver) = mpsc::channel(256);
    tokio::spawn(async move {
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr
                .take((MAX_LOG_LINE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await;
            String::from_utf8_lossy(&bytes).trim().to_owned()
        });
        let mut lines =
            FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_LOG_LINE_BYTES));
        let mut cancelled = false;
        loop {
            tokio::select! {
                _ = sender.closed() => {
                    cancelled = true;
                    break;
                }
                line = lines.next() => {
                    match line {
                        Some(Ok(line)) => {
                            if sender.send(LogMessage::Line { line }).await.is_err() {
                                cancelled = true;
                                break;
                            }
                        }
                        Some(Err(error)) => {
                            if sender.send(LogMessage::Error { message: format!("journal line rejected: {error}") }).await.is_err() {
                                cancelled = true;
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        if cancelled {
            let _ = child.start_kill();
        }
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let stderr = stderr_task.await.unwrap_or_default();
        if cancelled {
            return;
        }
        match status {
            Ok(Ok(status)) if status.success() => {
                let _ = sender
                    .send(LogMessage::Status {
                        message: "journal stream ended".to_owned(),
                    })
                    .await;
            }
            Ok(Ok(status)) => {
                let message = if stderr.is_empty() {
                    format!("journalctl exited with {status}")
                } else {
                    format!("journalctl failed: {stderr}")
                };
                let _ = sender.send(LogMessage::Error { message }).await;
            }
            Ok(Err(error)) => {
                let _ = sender
                    .send(LogMessage::Error {
                        message: format!("journal process error: {error}"),
                    })
                    .await;
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = sender
                    .send(LogMessage::Error {
                        message: "journal process did not stop".to_owned(),
                    })
                    .await;
            }
        }
    });
    Ok(receiver)
}

async fn docker_stream(
    service: crate::docker::DockerService,
    container: &str,
    tail: usize,
    since: i64,
) -> ApiResult<mpsc::Receiver<LogMessage>> {
    let mut body = service
        .follow_logs(container, tail, since)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let (sender, receiver) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut decoder = DockerLogDecoder::default();
        loop {
            tokio::select! {
                _ = sender.closed() => break,
                frame = body.frame() => {
                    match frame {
                        Some(Ok(frame)) => {
                            let Ok(data) = frame.into_data() else { continue; };
                            match decoder.push(&data) {
                                Ok(lines) => {
                                    for line in lines {
                                        if sender.send(LogMessage::Line { line }).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = sender.send(LogMessage::Error { message: error.to_string() }).await;
                                    return;
                                }
                            }
                        }
                        Some(Err(error)) => {
                            let _ = sender.send(LogMessage::Error { message: format!("Docker log stream failed: {error}") }).await;
                            return;
                        }
                        None => {
                            match decoder.finish() {
                                Ok(lines) => {
                                    for line in lines {
                                        if sender.send(LogMessage::Line { line }).await.is_err() {
                                            return;
                                        }
                                    }
                                    let _ = sender.send(LogMessage::Status { message: "Docker log stream ended".to_owned() }).await;
                                }
                                Err(error) => {
                                    let _ = sender.send(LogMessage::Error { message: error.to_string() }).await;
                                }
                            }
                            return;
                        }
                    }
                }
            }
        }
    });
    Ok(receiver)
}

#[derive(Default)]
struct DockerLogDecoder {
    mode: DockerStreamMode,
    pending: Vec<u8>,
    line: Vec<u8>,
    line_truncated: bool,
}

#[derive(Default, PartialEq, Eq)]
enum DockerStreamMode {
    #[default]
    Unknown,
    Raw,
    Multiplexed,
}

impl DockerLogDecoder {
    fn push(&mut self, data: &[u8]) -> anyhow::Result<Vec<String>> {
        if self.mode == DockerStreamMode::Raw {
            return Ok(self.push_text(data));
        }
        if self.pending.len().saturating_add(data.len()) > MAX_DOCKER_BUFFER_BYTES {
            anyhow::bail!("Docker log buffer limit exceeded");
        }
        self.pending.extend_from_slice(data);
        if self.mode == DockerStreamMode::Unknown {
            if self.pending.len() < 8 {
                return Ok(Vec::new());
            }
            if docker_header(&self.pending).is_some() {
                self.mode = DockerStreamMode::Multiplexed;
            } else {
                self.mode = DockerStreamMode::Raw;
                let bytes = std::mem::take(&mut self.pending);
                return Ok(self.push_text(&bytes));
            }
        }

        let mut output = Vec::new();
        loop {
            if self.pending.len() < 8 {
                break;
            }
            let length = docker_header(&self.pending)
                .ok_or_else(|| anyhow::anyhow!("invalid Docker multiplexed log header"))?;
            anyhow::ensure!(
                length <= MAX_DOCKER_FRAME_BYTES,
                "Docker log frame limit exceeded"
            );
            if self.pending.len() < 8 + length {
                break;
            }
            let payload = self.pending[8..8 + length].to_vec();
            self.pending.drain(..8 + length);
            output.extend(self.push_text(&payload));
        }
        Ok(output)
    }

    fn push_text(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut output = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                let mut line = String::from_utf8_lossy(&self.line).into_owned();
                if self.line_truncated {
                    line.push_str(" … [truncated]");
                }
                output.push(line);
                self.line.clear();
                self.line_truncated = false;
            } else if self.line.len() < MAX_LOG_LINE_BYTES {
                self.line.push(*byte);
            } else {
                self.line_truncated = true;
            }
        }
        output
    }

    fn finish(&mut self) -> anyhow::Result<Vec<String>> {
        if self.mode == DockerStreamMode::Unknown && !self.pending.is_empty() {
            self.mode = DockerStreamMode::Raw;
            let bytes = std::mem::take(&mut self.pending);
            let mut lines = self.push_text(&bytes);
            lines.extend(self.finish()?);
            return Ok(lines);
        }
        if self.mode == DockerStreamMode::Multiplexed && !self.pending.is_empty() {
            anyhow::bail!("Docker log stream ended with an incomplete frame");
        }
        if self.line.is_empty() && !self.line_truncated {
            return Ok(Vec::new());
        }
        let mut line = String::from_utf8_lossy(&self.line).into_owned();
        if self.line_truncated {
            line.push_str(" … [truncated]");
        }
        self.line.clear();
        self.line_truncated = false;
        Ok(vec![line])
    }
}

fn docker_header(bytes: &[u8]) -> Option<usize> {
    (bytes.len() >= 8 && matches!(bytes[0], 0..=2) && bytes[1..4] == [0, 0, 0])
        .then(|| u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_multiplexed_docker_frames() {
        let mut decoder = DockerLogDecoder::default();
        assert!(decoder.push(&[1, 0, 0]).unwrap().is_empty());
        assert!(decoder.push(&[0, 0, 0, 0, 6, b'h']).unwrap().is_empty());
        assert_eq!(decoder.push(b"ello\n").unwrap(), vec!["hello"]);
    }

    #[test]
    fn decodes_raw_docker_logs_and_flushes_partial_line() {
        let mut decoder = DockerLogDecoder::default();
        assert_eq!(decoder.push(b"first\nsecond").unwrap(), vec!["first"]);
        assert_eq!(decoder.finish().unwrap(), vec!["second"]);
    }

    #[test]
    fn rejects_oversized_docker_frames_before_allocating_payload() {
        let mut decoder = DockerLogDecoder::default();
        let length = u32::try_from(MAX_DOCKER_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes();
        let mut header = vec![1, 0, 0, 0];
        header.extend_from_slice(&length);
        assert!(decoder.push(&header).is_err());
    }

    #[test]
    fn rejects_incomplete_multiplexed_frame_at_eof() {
        let mut decoder = DockerLogDecoder::default();
        assert!(
            decoder
                .push(&[1, 0, 0, 0, 0, 0, 0, 8, b'x'])
                .unwrap()
                .is_empty()
        );
        assert!(decoder.finish().is_err());
    }

    #[tokio::test]
    async fn log_service_enforces_stream_limit() {
        let service = LogService::new(1);
        let permit = service.acquire().unwrap();
        assert!(service.acquire().is_err());
        drop(permit);
        assert!(service.acquire().is_ok());
    }
}
