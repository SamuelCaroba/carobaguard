pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod db;
pub mod docker;
pub mod doctor;
pub mod error;
pub mod services;
pub mod telemetry;

use std::sync::Arc;

use config::Config;
use docker::DockerService;
use services::SystemdService;
use sqlx::SqlitePool;
use telemetry::TelemetryService;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub telemetry: TelemetryService,
    pub docker: DockerService,
    pub systemd: SystemdService,
}
