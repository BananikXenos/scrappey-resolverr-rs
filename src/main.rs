use anyhow::Result;
use log::{error, info};
use transparent::TransparentChild;

mod browser;
mod challenge;
mod config;
mod flaresolverr;
mod fwd_proxy;
mod logging;
mod scrappey;
mod session;
use config::ServerConfig;
use flaresolverr::FlareSolverrAPI;

use crate::scrappey::ScrappeyClient;

/// Default proxy bridge bind address. Loopback-only: Chrome inside the
/// container/host shares the network namespace, so 127.0.0.1 is reachable
/// and we avoid exposing an authenticated-proxy relay if port 8080 is ever
/// mapped or the binary is run outside Docker.
const PROXY_BRIDGE_ADDR: &str = "127.0.0.1:8080";

/// Default chromedriver path
const CHROMEDRIVER_PATH: &str = "/usr/bin/chromedriver";

/// Default chromedriver port
const CHROMEDRIVER_PORT: u16 = 9515;

/// Default timeout for Scrappey balance check (seconds)
const SCRAPPEY_BALANCE_TIMEOUT: u64 = 30;

/// Entrypoint for the FlareSolverr-compatible server.
/// Initializes logging, loads config, starts proxy bridge, launches chromedriver, and runs the API server.
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize env_logger with better formatting
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .format_module_path(false)
        .format_target(false)
        .init();

    // Load configuration from environment variables
    let config = config::load_from_env()?;

    // Print scrappey API balance
    info!("Initializing Scrappey client and checking API balance...");
    let scrappey_client = ScrappeyClient::new(config.scrappey.api_key.clone());
    match scrappey_client.get_balance(SCRAPPEY_BALANCE_TIMEOUT).await {
        Ok(balance) => info!(
            "Scrappey API balance: {:.2} requests remaining",
            balance.balance
        ),
        Err(e) => {
            error!("Failed to get Scrappey API balance: {e}");
            use crate::logging::log_error_with_context;
            log_error_with_context(
                &crate::logging::LogContext::new().with_operation("scrappey_balance"),
                &e,
            );
        }
    }

    // Start the local proxy bridge in the background
    start_proxy_bridge(&config).await?;

    // Start the chromedriver process (for browser automation)
    let mut chromedriver = start_chromedriver()?;

    // Run the Axum API server and handle graceful shutdown
    run_server(config, &mut chromedriver).await?;

    Ok(())
}

/// Start the HTTP-to-HTTP proxy bridge in a background task.
/// This bridge allows the browser to use a local proxy that forwards to an upstream proxy (with optional auth).
async fn start_proxy_bridge(config: &ServerConfig) -> Result<()> {
    use crate::fwd_proxy::{FwdProxyConfig, HttpProxyBridge};

    // Convert our config to the fwd_proxy module's config
    let proxy_config = match (&config.proxy.username, &config.proxy.password) {
        (Some(username), Some(password)) => FwdProxyConfig::with_auth(
            config.proxy.host.clone(),
            config.proxy.port,
            username.clone(),
            password.clone(),
        ),
        _ => FwdProxyConfig::new(config.proxy.host.clone(), config.proxy.port),
    };

    // Bind and spawn the proxy bridge server
    info!("Starting HTTP proxy bridge on {}", PROXY_BRIDGE_ADDR);
    let mut bridge = HttpProxyBridge::new(proxy_config);
    bridge.bind(PROXY_BRIDGE_ADDR.parse()?).await?;
    info!(
        "Proxy bridge bound successfully, forwarding to {}:{}",
        config.proxy.host, config.proxy.port
    );
    tokio::spawn(async move {
        if let Err(e) = bridge.serve().await {
            error!("Proxy bridge server error: {e}");
            use crate::logging::log_error_with_context;
            log_error_with_context(
                &crate::logging::LogContext::new().with_operation("proxy_bridge"),
                &e,
            );
        }
    });
    Ok(())
}

/// Start the chromedriver process for browser automation.
/// Uses transparent process spawning for proper signal handling.
fn start_chromedriver() -> Result<TransparentChild> {
    use std::process::Command;
    use transparent::{CommandExt, TransparentRunner};

    info!("Starting chromedriver on port {}", CHROMEDRIVER_PORT);
    let chromedriver = Command::new(CHROMEDRIVER_PATH)
        .arg(format!("--port={}", CHROMEDRIVER_PORT))
        .spawn_transparent(&TransparentRunner::new())
        .map_err(|e| {
            error!(
                "Failed to start chromedriver at {}: {}",
                CHROMEDRIVER_PATH, e
            );
            anyhow::anyhow!(
                "Failed to start chromedriver at {}: {}",
                CHROMEDRIVER_PATH,
                e
            )
        })?;
    info!("Chromedriver started successfully");
    Ok(chromedriver)
}

/// Create a shutdown signal handler that waits for SIGINT or SIGTERM.
/// Returns a future that completes when a shutdown signal is received.
async fn shutdown_signal() {
    use tokio::signal;

    // Wait for either SIGINT or SIGTERM
    let ctrl_c = signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(e) => {
                error!("Failed to register SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = async { std::future::pending::<()>().await };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received, shutting down...");
}

/// Run the Axum API server with graceful shutdown and chromedriver cleanup.
/// Binds to the configured address, serves requests, and handles SIGINT/SIGTERM for shutdown.
async fn run_server(config: ServerConfig, chromedriver: &mut std::process::Child) -> Result<()> {
    use tokio::net::TcpListener;

    let addr = config.bind_address();
    info!("FlareSolverr API server starting on {}", addr);
    info!("Session data directory: {}", config.data_path);

    // Create FlareSolverr API instance and router
    let api = FlareSolverrAPI::new(config.clone());
    let app = api.create_router();

    // Create the TCP listener
    let listener = TcpListener::bind(&addr).await?;

    // Start the server with graceful shutdown
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    // Wait for the server to finish
    server.await?;

    // Stop chromedriver when the server stops
    info!("Shutting down chromedriver...");
    if let Err(e) = chromedriver.kill() {
        error!("Failed to kill chromedriver: {e}");
    } else {
        info!("Chromedriver stopped successfully");
    }

    Ok(())
}
