use anyhow::Result;
use log::{error, info};
use transparent::TransparentChild;

// Module imports for browser automation, challenge handling, API server, proxy bridge, and Scrappey integration.
mod browser;
mod challenge;
mod flaresolverr;
mod fwd_proxy;
mod scrappey;
use flaresolverr::{FlareSolverrAPI, FlareSolverrConfig};

use crate::scrappey::ScrappeyClient;

/// Entrypoint for the FlareSolverr-compatible server.
/// Initializes logging, loads config, starts proxy bridge, launches chromedriver, and runs the API server.
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize env_logger for logging support
    env_logger::init();

    // Load configuration from environment variables
    let config = load_config()?;

    // Print scrappey API balance
    info!("Checking Scrappey API balance...");
    let scrappey_client = ScrappeyClient::new(config.scrappey_api_key.clone());
    match scrappey_client.get_balance(30).await {
        Ok(balance) => info!("Scrappey API balance: {}", balance.balance),
        Err(e) => error!("Failed to get Scrappey API balance: {e}"),
    }

    // Start the local proxy bridge in the background
    start_proxy_bridge(&config).await?;

    // Start the chromedriver process (for browser automation)
    let mut chromedriver = start_chromedriver()?;

    // Run the Axum API server and handle graceful shutdown
    run_server(config, &mut chromedriver).await?;

    Ok(())
}

/// Load configuration from environment variables
/// Load configuration from environment variables.
/// Returns a FlareSolverrConfig struct or an error if required variables are missing.
fn load_config() -> Result<FlareSolverrConfig> {
    let scrappey_api_key = std::env::var("SCRAPPEY_API_KEY")?;
    let proxy_host = std::env::var("PROXY_HOST")?;
    let proxy_port = std::env::var("PROXY_PORT")?
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid PROXY_PORT"))?;
    let proxy_username = std::env::var("PROXY_USERNAME").ok();
    let proxy_password = std::env::var("PROXY_PASSWORD").ok();
    let data_path =
        std::env::var("DATA_PATH").unwrap_or_else(|_| "/data/persistent.json".to_string());

    Ok(FlareSolverrConfig {
        proxy_host,
        proxy_port,
        proxy_username,
        proxy_password,
        scrappey_api_key,
        data_path,
    })
}

/// Start the proxy bridge in a background task
/// Start the HTTP-to-HTTP proxy bridge in a background task.
/// This bridge allows the browser to use a local proxy that forwards to an upstream proxy (with optional auth).
async fn start_proxy_bridge(config: &FlareSolverrConfig) -> Result<()> {
    use crate::fwd_proxy::{HttpProxyBridge, ProxyConfig};

    // Build proxy config with or without authentication
    let proxy_config = if config.proxy_username.is_some() && config.proxy_password.is_some() {
        ProxyConfig::with_auth(
            config.proxy_host.clone(),
            config.proxy_port,
            config.proxy_username.as_ref().unwrap().clone(),
            config.proxy_password.as_ref().unwrap().clone(),
        )
    } else {
        ProxyConfig::new(config.proxy_host.clone(), config.proxy_port)
    };

    // Bind and spawn the proxy bridge server
    let mut bridge = HttpProxyBridge::new(proxy_config);
    bridge.bind("0.0.0.0:8080".parse()?).await?;
    tokio::spawn(async move {
        if let Err(e) = bridge.serve().await {
            error!("Error running proxy bridge: {e}");
        }
    });
    Ok(())
}

/// Start the chromedriver process
/// Start the chromedriver process for browser automation.
/// Uses transparent process spawning for proper signal handling.
fn start_chromedriver() -> Result<TransparentChild> {
    use std::process::Command;
    use transparent::{CommandExt, TransparentRunner};

    let chromedriver = Command::new("/usr/bin/chromedriver")
        .arg("--port=9515")
        .spawn_transparent(&TransparentRunner::new())
        .expect("Failed to start chromedriver");
    Ok(chromedriver)
}

/// Run the Axum server with graceful shutdown and chromedriver cleanup
/// Run the Axum API server with graceful shutdown and chromedriver cleanup.
/// Binds to the configured address, serves requests, and handles SIGINT/SIGTERM for shutdown.
async fn run_server(
    config: FlareSolverrConfig,
    chromedriver: &mut std::process::Child,
) -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::net::TcpListener;
    use tokio::signal;

    // Get host and port from environment or use defaults
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8191".to_string())
        .parse::<u16>()
        .unwrap_or(8191);

    let addr = format!("{host}:{port}");
    info!("FlareSolverr starting on {addr}");

    // Create FlareSolverr API instance and router
    let api = FlareSolverrAPI::new(config.clone());
    let app = api.create_router();

    // Create the TCP listener
    let listener = TcpListener::bind(&addr).await?;

    // Setup shutdown flag for graceful shutdown
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_flag_clone = shutdown_flag.clone();

    // Define shutdown signal handler (SIGINT/SIGTERM)
    let shutdown_signal = async move {
        // Wait for either SIGINT or SIGTERM
        let ctrl_c = signal::ctrl_c();
        #[cfg(unix)]
        let terminate = {
            let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
            async move { sigterm.recv().await }
        };
        #[cfg(not(unix))]
        let terminate = async { std::future::pending::<()>().await };

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
        shutdown_flag_clone.store(true, Ordering::SeqCst);
        info!("Shutdown signal received, shutting down...");
    };

    // Start the server with graceful shutdown
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal);

    // Wait for the server to finish
    server.await?;

    // Stop chromedriver when the server stops
    if let Err(e) = chromedriver.kill() {
        error!("Failed to kill chromedriver: {e}");
    }

    Ok(())
}
