pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod context;
pub mod db;
pub mod docker;
pub mod doctor;
pub mod error;
pub mod logs;
pub mod opencode;
pub mod services;
pub mod telemetry;
pub mod terminal;

use std::sync::Arc;

use config::Config;
use docker::DockerService;
use logs::LogService;
use opencode::OpenCodeManager;
use services::SystemdService;
use sqlx::SqlitePool;
use telemetry::TelemetryService;
use terminal::TerminalService;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub telemetry: TelemetryService,
    pub docker: DockerService,
    pub systemd: SystemdService,
    pub opencode: OpenCodeManager,
    pub terminal: TerminalService,
    pub logs: LogService,
}
