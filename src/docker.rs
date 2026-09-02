use std::{collections::HashMap, path::PathBuf, time::Instant};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, Request, StatusCode},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use hyperlocal::{UnixConnector, Uri};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    audit::{self, NewAuditEvent},
    auth::{self, AuthUser, Role},
    error::{ApiError, ApiResult},
};

const MAX_DOCKER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

type DockerClient = Client<UnixConnector, Full<Bytes>>;

#[derive(Clone)]
pub struct DockerService {
    socket: PathBuf,
    client: DockerClient,
}

impl Default for DockerService {
    fn default() -> Self {
        let socket = std::env::var_os("CAROBAGUARD_DOCKER_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"));
        let client = Client::builder(TokioExecutor::new()).build(UnixConnector);
        Self { socket, client }
    }
}

#[derive(Debug, Serialize)]
pub struct DockerStatus {
    pub available: bool,
    pub socket: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    #[serde(rename = "Version")]
    version: String,
}

#[derive(Debug, Deserialize)]
struct RawContainer {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Image")]
    image: String,
    #[serde(rename = "ImageID", default)]
    image_id: String,
    #[serde(rename = "Command", default)]
    command: String,
    #[serde(rename = "Created")]
    created: i64,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "Labels", default)]
    labels: HashMap<String, String>,
    #[serde(rename = "Ports", default)]
    ports: Vec<ContainerPort>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContainerPort {
    #[serde(rename = "IP", default)]
    pub ip: String,
    #[serde(rename = "PrivatePort")]
    pub private_port: u16,
    #[serde(rename = "PublicPort")]
    pub public_port: Option<u16>,
    #[serde(rename = "Type")]
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub command: String,
    pub created: i64,
    pub state: String,
    pub status: String,
    pub compose_project: Option<String>,
    pub health: Option<String>,
    pub labels: HashMap<String, String>,
    pub ports: Vec<ContainerPort>,
}

#[derive(Debug, Default, Serialize)]
pub struct ContainerStats {
    pub cpu_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_limit_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
    pub block_read_bytes: u64,
    pub block_write_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    tail: Option<usize>,
    since: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Kill,
}

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    action: ContainerAction,
}

impl DockerService {
    pub async fn status(&self) -> DockerStatus {
        match self.get_json::<VersionResponse>("/version").await {
            Ok(version) => DockerStatus {
                available: true,
                socket: self.socket.display().to_string(),
                version: Some(version.version),
                error: None,
            },
            Err(error) => DockerStatus {
                available: false,
                socket: self.socket.display().to_string(),
                version: None,
                error: Some(error.to_string()),
            },
        }
    }

    pub async fn containers(&self) -> anyhow::Result<Vec<ContainerSummary>> {
        let containers: Vec<RawContainer> = self.get_json("/containers/json?all=true").await?;
        Ok(containers
            .into_iter()
            .map(|container| {
                let name = container
                    .names
                    .first()
                    .map(|name| name.trim_start_matches('/').to_owned())
                    .unwrap_or_else(|| container.id.chars().take(12).collect());
                let health = parse_health(&container.status);
                let compose_project = container.labels.get("com.docker.compose.project").cloned();
                ContainerSummary {
                    id: container.id,
                    name,
                    image: container.image,
                    image_id: container.image_id,
                    command: container.command,
                    created: container.created,
                    state: container.state,
                    status: container.status,
                    compose_project,
                    health,
                    labels: container.labels,
                    ports: container.ports,
                }
            })
            .collect())
    }

    pub async fn inspect(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        validate_container_id(id)?;
        self.get_json(&format!("/containers/{id}/json")).await
    }

    pub async fn stats(&self, id: &str) -> anyhow::Result<ContainerStats> {
        validate_container_id(id)?;
        let value: serde_json::Value = self
            .get_json(&format!(
                "/containers/{id}/stats?stream=false&one-shot=true"
            ))
            .await?;
        Ok(parse_stats(&value))
    }

    pub async fn logs(&self, id: &str, tail: usize, since: i64) -> anyhow::Result<String> {
        validate_container_id(id)?;
        let tail = tail.clamp(1, 2000);
        let path = format!(
            "/containers/{id}/logs?stdout=true&stderr=true&timestamps=true&tail={tail}&since={}",
            since.max(0)
        );
        let body = self.request(Method::GET, &path, Bytes::new()).await?;
        Ok(decode_docker_log_stream(&body))
    }

    pub async fn action(&self, id: &str, action: &ContainerAction) -> anyhow::Result<()> {
        validate_container_id(id)?;
        let operation = match action {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop?t=10",
            ContainerAction::Restart => "restart?t=10",
            ContainerAction::Kill => "kill",
        };
        self.request(
            Method::POST,
            &format!("/containers/{id}/{operation}"),
            Bytes::new(),
        )
        .await?;
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let body = self.request(Method::GET, path, Bytes::new()).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    async fn request(&self, method: Method, path: &str, body: Bytes) -> anyhow::Result<Vec<u8>> {
        let uri: hyper::Uri = Uri::new(&self.socket, path).into();
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Full::new(body))?;
        let response = self.client.request(request).await?;
        let status = response.status();
        let response_body =
            collect_limited(response.into_body(), MAX_DOCKER_RESPONSE_BYTES).await?;
        if !status.is_success() {
            let message = serde_json::from_slice::<serde_json::Value>(&response_body)
                .ok()
                .and_then(|value| value["message"].as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&response_body).into_owned());
            anyhow::bail!("Docker API returned {status}: {message}");
        }
        Ok(response_body)
    }
}

async fn collect_limited(
    mut body: hyper::body::Incoming,
    maximum: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if output.len().saturating_add(data.len()) > maximum {
            anyhow::bail!("Docker API response exceeded {maximum} bytes");
        }
        output.extend_from_slice(&data);
    }
    Ok(output)
}

fn validate_container_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')),
        "invalid container identifier"
    );
    Ok(())
}

fn parse_health(status: &str) -> Option<String> {
    let start = status.find("(health:")? + "(health:".len();
    let end = status[start..].find(')')? + start;
    Some(status[start..end].trim().to_owned())
}

fn parse_stats(value: &serde_json::Value) -> ContainerStats {
    let number = |path: &[&str]| {
        path.iter()
            .try_fold(value, |current, key| current.get(*key))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let cpu_delta = number(&["cpu_stats", "cpu_usage", "total_usage"]).saturating_sub(number(&[
        "precpu_stats",
        "cpu_usage",
        "total_usage",
    ]));
    let system_delta = number(&["cpu_stats", "system_cpu_usage"])
        .saturating_sub(number(&["precpu_stats", "system_cpu_usage"]));
    let online_cpus = number(&["cpu_stats", "online_cpus"]).max(1);
    let cpu_percent = if system_delta == 0 {
        0.0
    } else {
        cpu_delta as f64 / system_delta as f64 * online_cpus as f64 * 100.0
    };
    let mut stats = ContainerStats {
        cpu_percent,
        memory_used_bytes: number(&["memory_stats", "usage"]).saturating_sub(number(&[
            "memory_stats",
            "stats",
            "inactive_file",
        ])),
        memory_limit_bytes: number(&["memory_stats", "limit"]),
        ..ContainerStats::default()
    };
    if let Some(networks) = value["networks"].as_object() {
        for network in networks.values() {
            stats.network_rx_bytes = stats
                .network_rx_bytes
                .saturating_add(network["rx_bytes"].as_u64().unwrap_or(0));
            stats.network_tx_bytes = stats
                .network_tx_bytes
                .saturating_add(network["tx_bytes"].as_u64().unwrap_or(0));
        }
    }
    if let Some(entries) = value["blkio_stats"]["io_service_bytes_recursive"].as_array() {
        for entry in entries {
            match entry["op"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "read" => {
                    stats.block_read_bytes = stats
                        .block_read_bytes
                        .saturating_add(entry["value"].as_u64().unwrap_or(0));
                }
                "write" => {
                    stats.block_write_bytes = stats
                        .block_write_bytes
                        .saturating_add(entry["value"].as_u64().unwrap_or(0));
                }
                _ => {}
            }
        }
    }
    stats
}

fn decode_docker_log_stream(bytes: &[u8]) -> String {
    if bytes.len() < 8 || !matches!(bytes[0], 0..=2) || bytes[1..4] != [0, 0, 0] {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut output = Vec::new();
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        offset += 8;
        if offset + length > bytes.len() {
            break;
        }
        output.extend_from_slice(&bytes[offset..offset + length]);
        offset += length;
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub async fn status(State(state): State<AppState>, _user: AuthUser) -> Json<DockerStatus> {
    Json(state.docker.status().await)
}

pub async fn containers(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<Vec<ContainerSummary>>> {
    Ok(Json(state.docker.containers().await.map_err(|error| {
        ApiError::service_unavailable(error.to_string())
    })?))
}

pub async fn inspect(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(state.docker.inspect(&id).await.map_err(|error| {
        ApiError::bad_request(error.to_string())
    })?))
}

pub async fn stats(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<ContainerStats>> {
    Ok(Json(state.docker.stats(&id).await.map_err(|error| {
        ApiError::bad_request(error.to_string())
    })?))
}

pub async fn logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> ApiResult<String> {
    state
        .docker
        .logs(&id, query.tail.unwrap_or(300), query.since.unwrap_or(0))
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

pub async fn action(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ActionRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    user.role.require(Role::Operator)?;
    auth::verify_csrf(&user, &headers)?;
    let started = Instant::now();
    let operation = match request.action {
        ContainerAction::Start => "start",
        ContainerAction::Stop => "stop",
        ContainerAction::Restart => "restart",
        ContainerAction::Kill => "kill",
    };
    let result = state.docker.action(&id, &request.action).await;
    let audit_result = if result.is_ok() { "success" } else { "failure" };
    audit::record(
        &state.db,
        NewAuditEvent {
            actor_user_id: Some(&user.id),
            actor_name: &user.username,
            origin: "manual",
            action: &format!("container.{operation}"),
            target: &id,
            command: Some(&format!("docker {operation} {id}")),
            result: audit_result,
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
    fn rejects_path_segments_in_container_identifiers() {
        assert!(validate_container_id("safe_name-1.2").is_ok());
        assert!(validate_container_id("../containers/json").is_err());
        assert!(validate_container_id("name?force=true").is_err());
    }

    #[test]
    fn decodes_multiplexed_docker_logs() {
        let mut bytes = vec![1, 0, 0, 0, 0, 0, 0, 6];
        bytes.extend_from_slice(b"hello\n");
        bytes.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 4]);
        bytes.extend_from_slice(b"err\n");
        assert_eq!(decode_docker_log_stream(&bytes), "hello\nerr\n");
    }

    #[test]
    fn calculates_container_stats_without_panics_on_missing_fields() {
        let stats = parse_stats(&serde_json::json!({
            "cpu_stats": {"cpu_usage": {"total_usage": 300}, "system_cpu_usage": 1000, "online_cpus": 2},
            "precpu_stats": {"cpu_usage": {"total_usage": 200}, "system_cpu_usage": 500},
            "memory_stats": {"usage": 1000, "limit": 4000, "stats": {"inactive_file": 100}}
        }));
        assert_eq!(stats.cpu_percent, 40.0);
        assert_eq!(stats.memory_used_bytes, 900);
    }
}
