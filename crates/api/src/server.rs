//! Server lifecycle, TCP binding, and graceful shutdown signal management.

use crate::config::ServerConfig;
use crate::routes::create_router;
use crate::state::AppState;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};

/// Starts and runs the HTTP Opening Explorer server until shutdown signal is received.
pub async fn run_server(config: ServerConfig) -> Result<(), anyhow::Error> {
    let state = AppState::new(config)?;
    let addr = format!("{}:{}", state.config.host, state.config.port);
    let listener = TcpListener::bind(&addr).await?;

    let actual_addr = listener.local_addr()?;
    info!("========================================================================");
    info!(
        "  Opening Explorer HTTP API listening on http://{}",
        actual_addr
    );
    info!("  Active collections: {:?}", state.ready_collections());
    info!("========================================================================");

    #[cfg(unix)]
    let state_for_hup = Arc::clone(&state);
    #[cfg(unix)]
    tokio::spawn(async move {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut hup_signal) => {
                while hup_signal.recv().await.is_some() {
                    info!("Received SIGHUP signal, initiating administrative dataset reload...");
                    match state_for_hup.reload().await {
                        Ok(reloaded) => {
                            info!("SIGHUP reload completed successfully for: {:?}", reloaded);
                        }
                        Err(e) => {
                            tracing::error!("SIGHUP reload rejected replacement snapshot: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to install SIGHUP signal handler: {}", e);
            }
        }
    });

    let router = create_router(state);

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!("Server runtime error: {}", e))?;

    info!("Server shutdown completed gracefully.");
    Ok(())
}

/// Awaits SIGINT (Ctrl-C) or SIGTERM for graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!("Failed to install Ctrl-C handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                warn!("Failed to install SIGTERM handler: {}", e);
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received SIGINT (Ctrl-C), initiating graceful shutdown...");
        }
        _ = terminate => {
            info!("Received SIGTERM, initiating graceful shutdown...");
        }
    }
}
