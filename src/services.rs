use std::{collections::HashMap, process::Stdio, time::Instant};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncReadExt, process::Command, time::Duration};

use crate::{
    AppState,
    audit::{self, NewAuditEvent},
    auth::{self, AuthUser, Role},
    error::{ApiError, ApiResult},
};

const MAX_COMMAND_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct SystemdService {
    systemctl: String,
    journalctl: String,
}

impl Default for SystemdService {
    fn default() -> Self {
        Self {
            systemctl: "systemctl".to_owned(),
            journalctl: "journalctl".to_owned(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SystemdStatus {
    pub available: bool,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawUnit {
    unit: String,
    load: String,
    active: String,
    sub: String,
    description: String,
}

#[derive(Debug, Serialize)]
pub struct UnitSummary {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
}

#[derive(Debug, Serialize)]
pub struct UnitDetails {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub unit_file_state: String,
    pub main_pid: u64,
    pub memory_current_bytes: Option<u64>,
    pub cpu_usage_nanoseconds: Option<u64>,
    pub active_enter_timestamp: String,
    pub fragment_path: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitAction {
    Start,
    Stop,
    Restart,
    Reload,
    Enable,
    Disable,
}

#[derive(Debug, Deserialize)]
pub struct UnitActionRequest {
    action: UnitAction,
}

#[derive(Debug, Deserialize)]
pub struct JournalQuery {
    lines: Option<usize>,
    since: Option<String>,
}

impl SystemdService {
    pub async fn status(&self) -> SystemdStatus {
        match Command::new(&self.systemctl)
            .args(["is-system-running", "--no-pager"])
            .output()
            .await
        {
            Ok(output) => SystemdStatus {
                available: true,
                state: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
                error: (!output.status.success())
                    .then(|| String::from_utf8_lossy(&output.stderr).trim().to_owned())
                    .filter(|value| !value.is_empty()),
            },
            Err(error) => SystemdStatus {
                available: false,
                state: "unavailable".to_owned(),
                error: Some(error.to_string()),
            },
        }
    }

    pub async fn units(&self) -> anyhow::Result<Vec<UnitSummary>> {
        let output = self
            .command_output(&[
                "list-units",
                "--type=service",
                "--all",
                "--no-pager",
                "--output=json",
            ])
            .await?;
        let raw: Vec<RawUnit> = serde_json::from_slice(&output)?;
        Ok(raw
            .into_iter()
            .map(|unit| UnitSummary {
                name: unit.unit,
                description: unit.description,
                load_state: unit.load,
                active_state: unit.active,
                sub_state: unit.sub,
            })
            .collect())
    }

    pub async fn details(&self, unit: &str) -> anyhow::Result<UnitDetails> {
        validate_unit(unit)?;
        let output = self
            .command_output(&[
                "show",
                unit,
                "--no-pager",
                "--property=Id,Description,LoadState,ActiveState,SubState,UnitFileState,MainPID,MemoryCurrent,CPUUsageNSec,ActiveEnterTimestamp,FragmentPath,Requires,Wants",
            ])
            .await?;
        let properties = parse_properties(&String::from_utf8_lossy(&output));
        let mut dependencies = properties
            .get("Requires")
            .into_iter()
            .chain(properties.get("Wants"))
            .flat_map(|value| value.split_whitespace())
            .filter(|value| value.ends_with(".service"))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        dependencies.sort();
        dependencies.dedup();
        Ok(UnitDetails {
            name: property(&properties, "Id"),
            description: property(&properties, "Description"),
            load_state: property(&properties, "LoadState"),
            active_state: property(&properties, "ActiveState"),
            sub_state: property(&properties, "SubState"),
            unit_file_state: property(&properties, "UnitFileState"),
            main_pid: parse_optional_number(&properties, "MainPID").unwrap_or(0),
            memory_current_bytes: parse_optional_number(&properties, "MemoryCurrent"),
            cpu_usage_nanoseconds: parse_optional_number(&properties, "CPUUsageNSec"),
            active_enter_timestamp: property(&properties, "ActiveEnterTimestamp"),
            fragment_path: property(&properties, "FragmentPath"),
            dependencies,
        })
    }

    pub async fn action(&self, unit: &str, action: &UnitAction) -> anyhow::Result<()> {
        validate_unit(unit)?;
        let operation = action_name(action);
        self.command_output(&[operation, unit, "--no-pager"])
            .await?;
        Ok(())
    }

    pub async fn logs(
        &self,
        unit: &str,
        lines: usize,
        since: Option<&str>,
    ) -> anyhow::Result<String> {
        validate_unit(unit)?;
        let lines = lines.clamp(1, 2000).to_string();
        let mut command = Command::new(&self.journalctl);
        command.args([
            "--unit",
            unit,
            "--lines",
            &lines,
            "--no-pager",
            "--output=short-iso-precise",
        ]);
        if let Some(since) = since {
            validate_since(since)?;
            command.args(["--since", since]);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut output = Vec::new();
        stdout
            .take((MAX_COMMAND_OUTPUT + 1) as u64)
            .read_to_end(&mut output)
            .await?;
        let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .map_err(|_| anyhow::anyhow!("journalctl timed out"))??;
        anyhow::ensure!(status.success(), "journalctl exited with {status}");
        anyhow::ensure!(
            output.len() <= MAX_COMMAND_OUTPUT,
            "journal response exceeded {MAX_COMMAND_OUTPUT} bytes"
        );
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    async fn command_output(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(&self.systemctl).args(args).output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("systemctl timed out"))??;
        anyhow::ensure!(
            output.stdout.len() <= MAX_COMMAND_OUTPUT && output.stderr.len() <= MAX_COMMAND_OUTPUT,
            "systemctl response exceeded {MAX_COMMAND_OUTPUT} bytes"
        );
        anyhow::ensure!(
            output.status.success(),
            "systemctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    }
}

fn validate_unit(unit: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        unit.ends_with(".service")
            && unit.len() <= 256
            && unit.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'-' | b'.' | b'@' | b':' | b'\\')
            }),
        "invalid systemd service name"
    );
    Ok(())
}

fn validate_since(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b' ' | b'-' | b':' | b'.' | b'+' | b'/' | b'_')
            }),
        "invalid journal since value"
    );
    Ok(())
}

fn parse_properties(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn property(properties: &HashMap<String, String>, key: &str) -> String {
    properties.get(key).cloned().unwrap_or_default()
}

fn parse_optional_number(properties: &HashMap<String, String>, key: &str) -> Option<u64> {
    properties
        .get(key)
        .filter(|value| value.as_str() != "[not set]")
        .and_then(|value| value.parse().ok())
}

fn action_name(action: &UnitAction) -> &'static str {
    match action {
        UnitAction::Start => "start",
        UnitAction::Stop => "stop",
        UnitAction::Restart => "restart",
        UnitAction::Reload => "reload",
        UnitAction::Enable => "enable",
        UnitAction::Disable => "disable",
    }
}

pub async fn status(State(state): State<AppState>, _user: AuthUser) -> Json<SystemdStatus> {
    Json(state.systemd.status().await)
}

pub async fn units(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<Vec<UnitSummary>>> {
    Ok(Json(state.systemd.units().await.map_err(|error| {
        ApiError::service_unavailable(error.to_string())
    })?))
}

pub async fn details(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(unit): Path<String>,
) -> ApiResult<Json<UnitDetails>> {
    Ok(Json(state.systemd.details(&unit).await.map_err(
        |error| ApiError::bad_request(error.to_string()),
    )?))
}

pub async fn logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(unit): Path<String>,
    Query(query): Query<JournalQuery>,
) -> ApiResult<String> {
    state
        .systemd
        .logs(&unit, query.lines.unwrap_or(300), query.since.as_deref())
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

pub async fn action(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(unit): Path<String>,
    Json(request): Json<UnitActionRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    user.role.require(Role::Operator)?;
    auth::verify_csrf(&user, &headers)?;
    let started = Instant::now();
    let operation = action_name(&request.action);
    let result = state.systemd.action(&unit, &request.action).await;
    audit::record(
        &state.db,
        NewAuditEvent {
            actor_user_id: Some(&user.id),
            actor_name: &user.username,
            origin: "manual",
            action: &format!("service.{operation}"),
            target: &unit,
            command: Some(&format!("systemctl {operation} {unit}")),
            result: if result.is_ok() { "success" } else { "failure" },
            duration_ms: started.elapsed().as_millis() as i64,
            exit_code: Some(if result.is_ok() { 0 } else { 1 }),
            ai_session_id: None,
            ai_permission_mode: None,
            metadata: serde_json::json!({}),
        },
    )
    .await
    .map_err(ApiError::internal)?;
    result.map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"ok": true}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_name_validation_blocks_option_and_path_injection() {
        assert!(validate_unit("sshd.service").is_ok());
        assert!(validate_unit("postgresql@main.service").is_ok());
        assert!(validate_unit("--root=/tmp.service").is_err());
        assert!(validate_unit("../../tmp/x.service").is_err());
        assert!(validate_unit("ssh.socket").is_err());
    }

    #[test]
    fn parses_systemd_show_without_shell_interpretation() {
        let properties = parse_properties("Id=sshd.service\nMainPID=42\nDescription=SSH daemon\n");
        assert_eq!(properties["MainPID"], "42");
        assert_eq!(properties["Description"], "SSH daemon");
    }
}
