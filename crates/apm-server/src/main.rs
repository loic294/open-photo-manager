//! Thin entry point: logging, configuration, state, serve until SIGINT/SIGTERM.

use std::time::Duration;

use anyhow::Context;
use apm_server::app;
use apm_server::config::Config;
use apm_server::state::AppState;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Open connections (SSE streams, slow downloads) are cut after this long once
/// a shutdown signal arrived.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    init_tracing(&config.log)?;

    let index = config.web_dir.join("index.html");
    if !index.is_file() {
        warn!(path = %index.display(), "web app not found; only /api will work (set APM_WEB_DIR)");
    }

    let state = AppState::new(config)
        .await
        .context("failed to initialise the application state")?;

    let listen = state.config.listen;
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind {listen}"))?;
    info!(
        address = %listener.local_addr().unwrap_or(listen),
        version = env!("CARGO_PKG_VERSION"),
        "apm-server listening"
    );

    let shutdown = CancellationToken::new();
    tokio::spawn({
        let shutdown = shutdown.clone();
        let state = state.clone();
        async move {
            wait_for_signal().await;
            info!("shutdown signal received");
            state.begin_shutdown();
            shutdown.cancel();
        }
    });

    let router = app::router(state.clone());
    let server = {
        let shutdown = shutdown.clone();
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
        }
    };
    let grace_expired = async {
        shutdown.cancelled().await;
        tokio::time::sleep(SHUTDOWN_GRACE).await;
    };
    tokio::select! {
        result = server => result.context("server error")?,
        () = grace_expired => warn!("graceful shutdown timed out; dropping open connections"),
    }

    state.shutdown().await;
    info!("apm-server stopped");
    Ok(())
}

fn init_tracing(directives: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(directives)
        .with_context(|| format!("invalid APM_LOG filter {directives:?}"))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialise logging: {err}"))
}

/// Resolves on SIGINT or (on Unix) SIGTERM, the signal `docker stop` sends.
async fn wait_for_signal() {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(%err, "cannot listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                warn!(%err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
