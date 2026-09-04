use std::{
    env,
    io::{ErrorKind, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ConnectInfo, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Uri, header},
    response::Response,
};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::{error, warn};
use uuid::Uuid;

use crate::{
    AppState, audit,
    auth::{self, AuthUser, Role},
    error::{ApiError, ApiResult},
};

const TERMINAL_PROTOCOL: &str = "carobaguard-v1";
const CSRF_PROTOCOL_PREFIX: &str = "csrf.";
const MAX_CLIENT_MESSAGE_BYTES: usize = 64 * 1024;
const WEBSOCKET_IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct TerminalService {
    slots: Arc<Semaphore>,
}

impl TerminalService {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(max_sessions)),
        }
    }

    fn acquire(&self) -> ApiResult<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_requests("terminal session limit reached"))
    }
}

#[derive(Deserialize)]
pub struct TerminalQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

pub async fn websocket(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Query(query): Query<TerminalQuery>,
    user: AuthUser,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    user.role.require(Role::Operator)?;
    verify_websocket_origin(&headers)?;
    let csrf_token = websocket_csrf_token(&headers)?;
    auth::verify_csrf_token(&user, csrf_token)?;
    let permit = state.terminal.acquire()?;
    let initial_size = terminal_size(query.cols.unwrap_or(100), query.rows.unwrap_or(30))?;

    Ok(ws
        .max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(MAX_CLIENT_MESSAGE_BYTES)
        .protocols([TERMINAL_PROTOCOL])
        .on_upgrade(move |socket| run_session(socket, state, user, address, initial_size, permit)))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage<'a> {
    Status {
        state: &'a str,
        session_id: &'a str,
        pid: Option<u32>,
    },
    Error {
        message: &'a str,
    },
    Exit {
        reason: &'a str,
        exit_code: Option<i32>,
    },
}

enum PtyOutput {
    Data(Vec<u8>),
    Error(String),
}

struct SpawnedTerminal {
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

async fn run_session(
    mut socket: WebSocket,
    state: AppState,
    user: AuthUser,
    address: SocketAddr,
    initial_size: PtySize,
    _permit: OwnedSemaphorePermit,
) {
    let session_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let shell = resolve_shell();
    let working_directory = resolve_working_directory();
    let spawned = tokio::task::spawn_blocking({
        let shell = shell.clone();
        let working_directory = working_directory.clone();
        move || spawn_terminal(&shell, &working_directory, initial_size)
    })
    .await;

    let mut terminal = match spawned {
        Ok(Ok(terminal)) => terminal,
        Ok(Err(error)) => {
            error!(%error, %session_id, "failed to create terminal PTY");
            record_open_failure(&state, &user, address, &session_id, started).await;
            send_server_message(
                &mut socket,
                &ServerMessage::Error {
                    message: "could not start the terminal",
                },
            )
            .await;
            return;
        }
        Err(error) => {
            error!(%error, %session_id, "terminal PTY task failed");
            record_open_failure(&state, &user, address, &session_id, started).await;
            send_server_message(
                &mut socket,
                &ServerMessage::Error {
                    message: "could not start the terminal",
                },
            )
            .await;
            return;
        }
    };

    let pid = terminal.child.process_id();
    if let Err(error) = audit::record(
        &state.db,
        audit::NewAuditEvent {
            actor_user_id: Some(&user.id),
            actor_name: &user.username,
            origin: "manual",
            action: "terminal.session.open",
            target: &session_id,
            command: Some(&shell.to_string_lossy()),
            result: "success",
            duration_ms: elapsed_millis(started),
            exit_code: None,
            ai_session_id: None,
            ai_permission_mode: None,
            metadata: serde_json::json!({
                "source_ip": address.ip().to_string(),
                "pid": pid,
                "cols": initial_size.cols,
                "rows": initial_size.rows,
            }),
        },
    )
    .await
    {
        error!(%error, %session_id, "terminal audit failed; closing session");
        send_server_message(
            &mut socket,
            &ServerMessage::Error {
                message: "the terminal could not be audited",
            },
        )
        .await;
        cleanup_child(terminal.master, terminal.child).await;
        return;
    }

    if !send_server_message(
        &mut socket,
        &ServerMessage::Status {
            state: "connected",
            session_id: &session_id,
            pid,
        },
    )
    .await
    {
        cleanup_child(terminal.master, terminal.child).await;
        record_close(
            &state,
            &user,
            &session_id,
            started,
            "client_disconnect",
            None,
            0,
            0,
        )
        .await;
        return;
    }

    let (output_tx, mut output_rx) = mpsc::channel::<PtyOutput>(64);
    let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(64);
    let reader = terminal.reader;
    let writer = terminal.writer;
    let reader_task = tokio::task::spawn_blocking({
        let output_tx = output_tx.clone();
        move || read_pty(reader, output_tx)
    });
    let writer_task = tokio::task::spawn_blocking(move || write_pty(writer, input_rx, output_tx));

    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_input = Instant::now();
    let mut input_bytes = 0_u64;
    let mut output_bytes = 0_u64;
    let mut exit_code = None;
    let reason = loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(message) = message else { break "client_disconnect"; };
                match message {
                    Ok(Message::Text(text)) => {
                        last_input = Instant::now();
                        let message = match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(message) => message,
                            Err(_) => break "protocol_error",
                        };
                        match message {
                            ClientMessage::Input { data } => {
                                if data.len() > MAX_CLIENT_MESSAGE_BYTES {
                                    break "protocol_error";
                                }
                                input_bytes = input_bytes.saturating_add(data.len() as u64);
                                if !matches!(
                                    tokio::time::timeout(WEBSOCKET_IO_TIMEOUT, input_tx.send(data.into_bytes())).await,
                                    Ok(Ok(()))
                                ) {
                                    break "pty_write_timeout";
                                }
                            }
                            ClientMessage::Resize { cols, rows } => {
                                let Ok(size) = terminal_size(cols, rows) else {
                                    break "protocol_error";
                                };
                                if let Err(error) = terminal.master.resize(size) {
                                    warn!(%error, %session_id, "terminal resize failed");
                                    break "pty_error";
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => break "client_disconnect",
                    Ok(Message::Ping(payload)) => {
                        if !matches!(
                            tokio::time::timeout(WEBSOCKET_IO_TIMEOUT, socket.send(Message::Pong(payload))).await,
                            Ok(Ok(()))
                        ) {
                            break "client_disconnect";
                        }
                    }
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Binary(_)) => break "protocol_error",
                    Err(_) => break "client_disconnect",
                }
            }
            output = output_rx.recv() => {
                match output {
                    Some(PtyOutput::Data(data)) => {
                        output_bytes = output_bytes.saturating_add(data.len() as u64);
                        if !matches!(
                            tokio::time::timeout(WEBSOCKET_IO_TIMEOUT, socket.send(Message::Binary(data.into()))).await,
                            Ok(Ok(()))
                        ) {
                            break "client_disconnect";
                        }
                    }
                    Some(PtyOutput::Error(error)) => {
                        warn!(%error, %session_id, "terminal PTY I/O failed");
                        break "pty_error";
                    }
                    None => break "pty_closed",
                }
            }
            _ = ticker.tick() => {
                match terminal.child.try_wait() {
                    Ok(Some(status)) => {
                        exit_code = i32::try_from(status.exit_code()).ok();
                        break "child_exit";
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(%error, %session_id, "failed to poll terminal child");
                        break "child_error";
                    }
                }
                if let Some(reason) = terminal_timeout_reason(
                    last_input.elapsed(),
                    started.elapsed(),
                    Duration::from_secs(state.config.terminal_idle_timeout_seconds),
                    Duration::from_secs(state.config.terminal_max_duration_seconds),
                ) {
                    break reason;
                }
            }
        }
    };

    send_server_message(&mut socket, &ServerMessage::Exit { reason, exit_code }).await;
    drop(input_tx);
    cleanup_child(terminal.master, terminal.child).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), writer_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), reader_task).await;
    record_close(
        &state,
        &user,
        &session_id,
        started,
        reason,
        exit_code,
        input_bytes,
        output_bytes,
    )
    .await;
}

fn spawn_terminal(
    shell: &Path,
    working_directory: &Path,
    size: PtySize,
) -> anyhow::Result<SpawnedTerminal> {
    let pair = native_pty_system().openpty(size)?;
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let mut command = CommandBuilder::new(shell);
    command.cwd(working_directory);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("CAROBAGUARD_TERMINAL", "1");
    let child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    Ok(SpawnedTerminal {
        master: pair.master,
        reader,
        writer,
        child,
    })
}

fn read_pty(mut reader: Box<dyn Read + Send>, output: mpsc::Sender<PtyOutput>) {
    let mut buffer = vec![0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if output
                    .blocking_send(PtyOutput::Data(buffer[..read].to_vec()))
                    .is_err()
                {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => {
                let _ = output.blocking_send(PtyOutput::Error(error.to_string()));
                break;
            }
        }
    }
}

fn write_pty(
    mut writer: Box<dyn Write + Send>,
    mut input: mpsc::Receiver<Vec<u8>>,
    output: mpsc::Sender<PtyOutput>,
) {
    while let Some(data) = input.blocking_recv() {
        if let Err(error) = writer.write_all(&data).and_then(|_| writer.flush()) {
            let _ = output.blocking_send(PtyOutput::Error(error.to_string()));
            break;
        }
    }
}

async fn cleanup_child(master: Box<dyn MasterPty + Send>, mut child: Box<dyn Child + Send + Sync>) {
    #[cfg(unix)]
    let process_group = master.process_group_leader();
    drop(master);
    let cleanup = tokio::task::spawn_blocking(move || {
        let child_is_running = matches!(child.try_wait(), Ok(None));
        #[cfg(unix)]
        if let Some(process_group) = process_group.filter(|pid| {
            // SAFETY: getpgrp has no preconditions and does not dereference pointers.
            let carobaguard_group = unsafe { libc::getpgrp() };
            child_is_running && *pid > 1 && *pid != carobaguard_group
        }) {
            // SAFETY: the group belongs to the still-live child handle and is neither
            // the CarobaGuard process group nor a special kill target.
            unsafe {
                libc::kill(-process_group, libc::SIGHUP);
            }
        }
        if child_is_running {
            let _ = child.kill();
        }
        let _ = child.wait();
    });
    if tokio::time::timeout(Duration::from_secs(3), cleanup)
        .await
        .is_err()
    {
        warn!("terminal child cleanup exceeded timeout");
    }
}

async fn send_server_message(socket: &mut WebSocket, message: &ServerMessage<'_>) -> bool {
    let Ok(serialized) = serde_json::to_string(message) else {
        return false;
    };
    matches!(
        tokio::time::timeout(
            WEBSOCKET_IO_TIMEOUT,
            socket.send(Message::Text(serialized.into())),
        )
        .await,
        Ok(Ok(()))
    )
}

async fn record_open_failure(
    state: &AppState,
    user: &AuthUser,
    address: SocketAddr,
    session_id: &str,
    started: Instant,
) {
    if let Err(error) = audit::record(
        &state.db,
        audit::NewAuditEvent {
            actor_user_id: Some(&user.id),
            actor_name: &user.username,
            origin: "manual",
            action: "terminal.session.open",
            target: session_id,
            command: None,
            result: "failure",
            duration_ms: elapsed_millis(started),
            exit_code: None,
            ai_session_id: None,
            ai_permission_mode: None,
            metadata: serde_json::json!({"source_ip": address.ip().to_string()}),
        },
    )
    .await
    {
        error!(%error, %session_id, "failed to audit terminal startup failure");
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_close(
    state: &AppState,
    user: &AuthUser,
    session_id: &str,
    started: Instant,
    reason: &str,
    exit_code: Option<i32>,
    input_bytes: u64,
    output_bytes: u64,
) {
    if let Err(error) = audit::record(
        &state.db,
        audit::NewAuditEvent {
            actor_user_id: Some(&user.id),
            actor_name: &user.username,
            origin: "manual",
            action: "terminal.session.close",
            target: session_id,
            command: None,
            result: if reason == "child_exit" && exit_code == Some(0) {
                "success"
            } else if matches!(
                reason,
                "client_disconnect" | "idle_timeout" | "max_duration"
            ) {
                "stopped"
            } else {
                "failure"
            },
            duration_ms: elapsed_millis(started),
            exit_code,
            ai_session_id: None,
            ai_permission_mode: None,
            metadata: terminal_close_metadata(reason, input_bytes, output_bytes),
        },
    )
    .await
    {
        error!(%error, %session_id, "failed to audit terminal close");
    }
}

fn terminal_size(cols: u16, rows: u16) -> ApiResult<PtySize> {
    if !(2..=500).contains(&cols) || !(1..=200).contains(&rows) {
        return Err(ApiError::bad_request("invalid terminal dimensions"));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn terminal_timeout_reason(
    idle_elapsed: Duration,
    session_elapsed: Duration,
    idle_timeout: Duration,
    max_duration: Duration,
) -> Option<&'static str> {
    if idle_elapsed >= idle_timeout {
        Some("idle_timeout")
    } else if session_elapsed >= max_duration {
        Some("max_duration")
    } else {
        None
    }
}

fn terminal_close_metadata(reason: &str, input_bytes: u64, output_bytes: u64) -> serde_json::Value {
    serde_json::json!({
        "reason": reason,
        "input_bytes": input_bytes,
        "output_bytes": output_bytes,
        "contents_recorded": false,
    })
}

fn resolve_shell() -> PathBuf {
    env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && is_executable(path))
        .or_else(|| {
            ["/bin/bash", "/usr/bin/bash", "/bin/sh"]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| is_executable(path))
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn resolve_working_directory() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn websocket_csrf_token(headers: &HeaderMap) -> ApiResult<&str> {
    let protocols = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("missing terminal protocol"))?;
    let mut has_version = false;
    let mut csrf = None;
    for protocol in protocols.split(',').map(str::trim) {
        if protocol == TERMINAL_PROTOCOL {
            has_version = true;
        } else if let Some(token) = protocol.strip_prefix(CSRF_PROTOCOL_PREFIX)
            && (token.is_empty() || csrf.replace(token).is_some())
        {
            return Err(ApiError::forbidden("invalid terminal protocol"));
        }
    }
    if !has_version {
        return Err(ApiError::forbidden("invalid terminal protocol"));
    }
    csrf.ok_or_else(|| ApiError::forbidden("missing CSRF token"))
}

fn verify_websocket_origin(headers: &HeaderMap) -> ApiResult<()> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("missing WebSocket origin"))?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("missing Host header"))?;
    let uri = origin
        .parse::<Uri>()
        .map_err(|_| ApiError::forbidden("invalid WebSocket origin"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri
            .authority()
            .is_none_or(|authority| !authority.as_str().eq_ignore_ascii_case(host))
        || uri.path() != "/"
        || uri.query().is_some()
    {
        return Err(ApiError::forbidden("cross-origin WebSocket denied"));
    }
    Ok(())
}

fn elapsed_millis(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn websocket_requires_exact_same_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("server.example:8090"),
        );
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://server.example:8090"),
        );
        assert!(verify_websocket_origin(&headers).is_ok());

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        assert!(verify_websocket_origin(&headers).is_err());
    }

    #[test]
    fn csrf_protocol_is_not_accepted_without_version_or_when_duplicated() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("carobaguard-v1, csrf.expected"),
        );
        assert_eq!(websocket_csrf_token(&headers).unwrap(), "expected");

        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("csrf.expected"),
        );
        assert!(websocket_csrf_token(&headers).is_err());

        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("carobaguard-v1, csrf.one, csrf.two"),
        );
        assert!(websocket_csrf_token(&headers).is_err());
    }

    #[test]
    fn terminal_dimensions_are_bounded() {
        assert!(terminal_size(80, 24).is_ok());
        assert!(terminal_size(1, 24).is_err());
        assert!(terminal_size(80, 201).is_err());
    }

    #[test]
    fn terminal_input_preserves_control_bytes_and_administrative_commands() {
        for data in [
            "\u{3}",
            "sudo whoami\r",
            "sudo systemctl status crafty\r",
            "sudo -i\r",
        ] {
            let encoded = serde_json::json!({"type": "input", "data": data});
            let ClientMessage::Input { data: decoded } =
                serde_json::from_value(encoded).expect("valid terminal input")
            else {
                panic!("input decoded as the wrong message type");
            };
            assert_eq!(decoded, data);
        }
    }

    #[test]
    fn terminal_timeouts_remain_bounded() {
        assert_eq!(
            terminal_timeout_reason(
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(300),
            ),
            Some("idle_timeout")
        );
        assert_eq!(
            terminal_timeout_reason(
                Duration::from_secs(1),
                Duration::from_secs(300),
                Duration::from_secs(60),
                Duration::from_secs(300),
            ),
            Some("max_duration")
        );
        assert_eq!(
            terminal_timeout_reason(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(60),
                Duration::from_secs(300),
            ),
            None
        );
    }

    #[test]
    fn terminal_audit_metadata_contains_counts_but_no_typed_content() {
        let metadata = terminal_close_metadata("client_disconnect", 14, 28);
        assert_eq!(metadata["input_bytes"], 14);
        assert_eq!(metadata["output_bytes"], 28);
        assert_eq!(metadata["contents_recorded"], false);
        assert_eq!(metadata.as_object().unwrap().len(), 4);
    }

    #[test]
    fn installer_allows_os_authorized_privilege_elevation_in_the_pty() {
        let installer = include_str!("../scripts/install-user.sh");
        assert!(
            !installer
                .lines()
                .any(|line| { line.trim_start().starts_with("NoNewPrivileges=") })
        );
        assert!(installer.contains("KillMode=control-group"));
        assert!(installer.contains("PrivateTmp=true"));
        assert!(installer.contains("UMask=0077"));
    }

    #[tokio::test]
    async fn terminal_service_enforces_and_releases_session_limit() {
        let service = TerminalService::new(1);
        let permit = service.acquire().unwrap();
        assert!(service.acquire().is_err());
        drop(permit);
        assert!(service.acquire().is_ok());
    }
}
