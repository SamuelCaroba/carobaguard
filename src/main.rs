use std::sync::Arc;

use anyhow::Context;
use carobaguard::{
    AppState, api, config::Config, db, docker::DockerService, opencode::OpenCodeManager,
    services::SystemdService, telemetry::TelemetryService,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("carobaguard=info,tower_http=info")),
        )
        .compact()
        .init();

    let config = Arc::new(Config::from_env()?);
    let db = db::connect(&config).await?;
    db::bootstrap_admin(&db).await?;

    let telemetry = TelemetryService::start(db.clone()).await?;
    let opencode = OpenCodeManager::new(db.clone());
    let state = AppState {
        config: config.clone(),
        db,
        telemetry,
        docker: DockerService::default(),
        systemd: SystemdService::default(),
        opencode,
    };

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("cannot bind CarobaGuard to {}", config.bind))?;

    info!(address = %config.bind, data_dir = %config.data_dir.display(), "CarobaGuard ready");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
