use axum::{
    Json,
    extract::{Query, State},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::{
    AppState,
    auth::AuthUser,
    error::{ApiError, ApiResult},
};

#[derive(Debug)]
pub struct NewAuditEvent<'a> {
    pub actor_user_id: Option<&'a str>,
    pub actor_name: &'a str,
    pub origin: &'a str,
    pub action: &'a str,
    pub target: &'a str,
    pub command: Option<&'a str>,
    pub result: &'a str,
    pub duration_ms: i64,
    pub exit_code: Option<i32>,
    pub ai_session_id: Option<&'a str>,
    pub ai_permission_mode: Option<&'a str>,
    pub metadata: serde_json::Value,
}

pub async fn record(pool: &SqlitePool, event: NewAuditEvent<'_>) -> anyhow::Result<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO audit_events(id, actor_user_id, actor_name, origin, action, target, \
         command, result, duration_ms, exit_code, ai_session_id, ai_permission_mode, \
         metadata_json, created_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(event.actor_user_id)
    .bind(event.actor_name)
    .bind(event.origin)
    .bind(event.action)
    .bind(event.target)
    .bind(event.command)
    .bind(event.result)
    .bind(event.duration_ms)
    .bind(event.exit_code)
    .bind(event.ai_session_id)
    .bind(event.ai_permission_mode)
    .bind(serde_json::to_string(&event.metadata)?)
    .bind(Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(id)
}

#[derive(Debug, FromRow, Serialize)]
pub struct AuditEvent {
    id: String,
    actor_user_id: Option<String>,
    actor_name: String,
    origin: String,
    action: String,
    target: String,
    command: Option<String>,
    result: String,
    duration_ms: i64,
    exit_code: Option<i32>,
    ai_session_id: Option<String>,
    ai_permission_mode: Option<String>,
    metadata_json: String,
    created_at: i64,
}

#[derive(Deserialize)]
pub struct AuditQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<Vec<AuditEvent>>> {
    let events = sqlx::query_as::<_, AuditEvent>(
        "SELECT id, actor_user_id, actor_name, origin, action, target, command, result, \
         duration_ms, exit_code, ai_session_id, ai_permission_mode, metadata_json, created_at \
         FROM audit_events ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(query.limit.unwrap_or(100).clamp(1, 500))
    .bind(query.offset.unwrap_or(0).max(0))
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, db};

    #[tokio::test]
    async fn stores_complete_mutation_record() {
        let temp = tempfile::tempdir().unwrap();
        let pool = db::connect(&Config::test(temp.path().to_path_buf()))
            .await
            .unwrap();
        let id = record(
            &pool,
            NewAuditEvent {
                actor_user_id: None,
                actor_name: "test",
                origin: "manual",
                action: "container.restart",
                target: "example",
                command: Some("docker restart example"),
                result: "success",
                duration_ms: 12,
                exit_code: Some(0),
                ai_session_id: None,
                ai_permission_mode: None,
                metadata: serde_json::json!({"reason": "test"}),
            },
        )
        .await
        .unwrap();
        let result: String = sqlx::query_scalar("SELECT result FROM audit_events WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(result, "success");
    }
}
