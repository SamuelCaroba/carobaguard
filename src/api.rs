use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};

use crate::{
    AppState,
    auth::{self, AuthUser, Role},
    error::{ApiError, ApiResult},
    telemetry::{SystemSnapshot, TelemetryProfile},
};

#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/login", post(auth::login))
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/metrics/current", get(current_metrics))
        .route("/api/v1/metrics/history", get(metric_history))
        .route("/api/v1/metrics/events", get(metric_events))
        .route("/api/v1/metrics/config", post(configure_metrics))
        .fallback(static_asset)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn current_metrics(State(state): State<AppState>, _user: AuthUser) -> Json<SystemSnapshot> {
    Json(state.telemetry.latest().await)
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

async fn metric_history(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<SystemSnapshot>>> {
    Ok(Json(
        state
            .telemetry
            .history(query.limit.unwrap_or(240))
            .await
            .map_err(ApiError::internal)?,
    ))
}

async fn metric_events(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = state.telemetry.subscribe();
    let telemetry = state.telemetry.clone();
    let events = stream! {
        let initial = telemetry.latest().await;
        if let Ok(json) = serde_json::to_string(&initial) {
            yield Ok(Event::default().event("metrics").data(json));
        }
        loop {
            match receiver.recv().await {
                Ok(sample) => {
                    if let Ok(json) = serde_json::to_string(&sample) {
                        yield Ok(Event::default().event("metrics").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(events).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("keep-alive"),
    )
}

#[derive(Deserialize)]
struct MetricsConfigRequest {
    profile: TelemetryProfile,
    performance_mode: bool,
}

#[derive(Serialize)]
struct MetricsConfigResponse {
    profile: TelemetryProfile,
    performance_mode: bool,
    interval_seconds: u64,
}

async fn configure_metrics(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(request): Json<MetricsConfigRequest>,
) -> ApiResult<Json<MetricsConfigResponse>> {
    user.role.require(Role::Operator)?;
    auth::verify_csrf(&user, &headers)?;
    let interval = state
        .telemetry
        .configure(request.profile, request.performance_mode)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(MetricsConfigResponse {
        profile: request.profile,
        performance_mode: request.performance_mode,
        interval_seconds: interval,
    }))
}

async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let requested = if path.is_empty() { "index.html" } else { path };
    let Some(asset) = WebAssets::get(requested).or_else(|| WebAssets::get("index.html")) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = mime_guess::from_path(requested).first_or_octet_stream();
    let mut response = Response::new(Body::from(asset.data));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
