use std::net::SocketAddr;

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};

const SESSION_COOKIE: &str = "carobaguard_session";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Operator,
    Viewer,
    AiAgent,
}

impl Role {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "operator" => Some(Self::Operator),
            "viewer" => Some(Self::Viewer),
            "ai_agent" => Some(Self::AiAgent),
            _ => None,
        }
    }

    pub fn require(self, required: Role) -> ApiResult<()> {
        let level = |role| match role {
            Role::Viewer => 0,
            Role::Operator => 1,
            Role::Admin => 2,
            Role::AiAgent => 0,
        };
        if self == Role::AiAgent || level(self) < level(required) {
            return Err(ApiError::forbidden(
                "this role cannot perform the operation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: Role,
    #[serde(skip)]
    pub session_id: String,
    #[serde(skip)]
    pub csrf_token: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(&state.db, &parts.headers).await
    }
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    user: AuthUser,
    csrf_token: String,
    expires_at: i64,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> ApiResult<Response> {
    if request.username.len() > 128 || request.password.len() > 4096 {
        return Err(ApiError::bad_request("invalid credentials"));
    }
    let source_ip = address.ip().to_string();
    enforce_login_rate_limit(&state.db, &source_ip).await?;

    let row: Option<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, username, password_hash, role, enabled FROM users WHERE username = ?",
    )
    .bind(&request.username)
    .fetch_optional(&state.db)
    .await?;

    let valid = if let Some((_, _, hash, _, enabled)) = &row {
        *enabled == 1 && verify_password(request.password.clone(), hash.clone()).await
    } else {
        let dummy = hash_password("invalid-credential-placeholder".to_owned())
            .await
            .map_err(ApiError::internal)?;
        let _ = verify_password(request.password.clone(), dummy).await;
        false
    };

    record_login_attempt(&state.db, &source_ip, &request.username, valid).await?;
    if !valid {
        return Err(ApiError::unauthorized("invalid username or password"));
    }

    let (user_id, username, _, role, _) = row.expect("validated user row must exist");
    let role = Role::parse(&role).ok_or_else(|| ApiError::internal("invalid stored role"))?;
    let token = random_token(32);
    let csrf_token = random_token(24);
    let session_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let expires_at = now + state.config.session_ttl_seconds;
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(512)
        .collect::<String>();
    sqlx::query(
        "INSERT INTO sessions(id, user_id, token_hash, csrf_token, source_ip, user_agent, \
         created_at, expires_at, last_seen_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id)
    .bind(&user_id)
    .bind(token_hash(&token))
    .bind(&csrf_token)
    .bind(&source_ip)
    .bind(user_agent)
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .execute(&state.db)
    .await?;

    let user = AuthUser {
        id: user_id,
        username,
        role,
        session_id,
        csrf_token: csrf_token.clone(),
    };
    let body = Json(SessionResponse {
        user,
        csrf_token,
        expires_at,
    });
    let mut response = body.into_response();
    let cookie = session_cookie(
        &token,
        state.config.session_ttl_seconds,
        state.config.cookie_secure,
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok(response)
}

pub async fn me(user: AuthUser) -> Json<SessionResponse> {
    Json(SessionResponse {
        csrf_token: user.csrf_token.clone(),
        expires_at: 0,
        user,
    })
}

pub async fn logout(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Response> {
    verify_csrf(&user, &headers)?;
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&user.session_id)
        .execute(&state.db)
        .await?;
    let mut response = Json(serde_json::json!({ "ok": true })).into_response();
    let secure = if state.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"
        ))
        .map_err(ApiError::internal)?,
    );
    Ok(response)
}

pub fn verify_csrf(user: &AuthUser, headers: &HeaderMap) -> ApiResult<()> {
    let supplied = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("missing CSRF token"))?;
    verify_csrf_token(user, supplied)
}

pub fn verify_csrf_token(user: &AuthUser, supplied: &str) -> ApiResult<()> {
    if token_hash(supplied) != token_hash(&user.csrf_token) {
        return Err(ApiError::forbidden("invalid CSRF token"));
    }
    Ok(())
}

async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> ApiResult<AuthUser> {
    let token = cookie_value(headers, SESSION_COOKIE)
        .ok_or_else(|| ApiError::unauthorized("authentication required"))?;
    let now = Utc::now().timestamp();
    let row: Option<(String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.username, u.role, s.id, s.csrf_token, s.last_seen_at \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ? AND s.expires_at > ? AND u.enabled = 1",
    )
    .bind(token_hash(&token))
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let (id, username, role, session_id, csrf_token, last_seen_at) =
        row.ok_or_else(|| ApiError::unauthorized("session is invalid or expired"))?;
    if now - last_seen_at > 300 {
        sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(&session_id)
            .execute(pool)
            .await?;
    }
    Ok(AuthUser {
        id,
        username,
        role: Role::parse(&role).ok_or_else(|| ApiError::internal("invalid stored role"))?,
        session_id,
        csrf_token,
    })
}

async fn enforce_login_rate_limit(pool: &SqlitePool, source_ip: &str) -> ApiResult<()> {
    let since = Utc::now().timestamp() - 900;
    let failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM login_attempts \
         WHERE source_ip = ? AND succeeded = 0 AND created_at > ?",
    )
    .bind(source_ip)
    .bind(since)
    .fetch_one(pool)
    .await?;
    if failures >= 5 {
        return Err(ApiError::too_many_requests(
            "too many failed logins; try again later",
        ));
    }
    Ok(())
}

async fn record_login_attempt(
    pool: &SqlitePool,
    source_ip: &str,
    username: &str,
    succeeded: bool,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO login_attempts(source_ip, username, succeeded, created_at) VALUES(?, ?, ?, ?)",
    )
    .bind(source_ip)
    .bind(username)
    .bind(succeeded)
    .bind(Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn session_cookie(token: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    )
}

fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

pub fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

pub async fn hash_password(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(anyhow::Error::msg)
    })
    .await?
}

async fn verify_password(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).ok().is_some_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passwords_use_salted_argon2() {
        let first = hash_password("correct horse battery staple".to_owned())
            .await
            .unwrap();
        let second = hash_password("correct horse battery staple".to_owned())
            .await
            .unwrap();
        assert_ne!(first, second);
        assert!(verify_password("correct horse battery staple".to_owned(), first).await);
        assert!(!verify_password("wrong".to_owned(), second).await);
    }

    #[test]
    fn cookie_parser_does_not_match_suffixes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("not_carobaguard_session=x; carobaguard_session=right"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("right")
        );
    }

    #[test]
    fn role_hierarchy_denies_viewers_and_ai_agents_mutation_access() {
        assert!(Role::Admin.require(Role::Operator).is_ok());
        assert!(Role::Operator.require(Role::Operator).is_ok());
        assert!(Role::Viewer.require(Role::Operator).is_err());
        assert!(Role::AiAgent.require(Role::Viewer).is_err());
    }
}
