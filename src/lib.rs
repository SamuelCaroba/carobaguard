pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod telemetry;

use std::sync::Arc;

use config::Config;
use sqlx::SqlitePool;
use telemetry::TelemetryService;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub telemetry: TelemetryService,
}
