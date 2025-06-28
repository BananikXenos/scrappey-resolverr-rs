use anyhow::Result;
use std::process::Command;
use tokio::net::TcpListener;
use transparent::{CommandExt, TransparentRunner};

mod browser;
mod challenge;
mod flaresolverr;
mod fwd_proxy;
mod scrappey;
use flaresolverr::{FlareSolverrAPI, FlareSolverrConfig};

use crate::fwd_proxy::{HttpProxyBridge, ProxyConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Extract environment variables
    let scrappey_api_key = std::env::var("SCRAPPEY_API_KEY")?;
    let proxy_host = std::env::var("PROXY_HOST")?;
    let proxy_port = std::env::var("PROXY_PORT")?
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid PROXY_PORT"))?;
    let proxy_username = std::env::var("PROXY_USERNAME").ok();
    let proxy_password = std::env::var("PROXY_PASSWORD").ok();
    let data_path =
        std::env::var("DATA_PATH").unwrap_or_else(|_| "/data/persistent.json".to_string());

    // Run local http to socks5 proxy server
    let proxy_config = if proxy_username.is_some() && proxy_password.is_some() {
        ProxyConfig::with_auth(
            proxy_host.clone(),
            proxy_port,
            proxy_username.as_ref().unwrap().clone(),
            proxy_password.as_ref().unwrap().clone(),
        )
    } else {
        ProxyConfig::new(proxy_host.clone(), proxy_port)
    };

    let mut bridge = HttpProxyBridge::new(proxy_config);
    bridge.bind("0.0.0.0:8080".parse()?).await?;
    tokio::spawn(async move {
        if let Err(e) = bridge.serve().await {
            eprintln!("Error running proxy bridge: {e}");
        }
    });

    let mut chromedriver = Command::new("/usr/bin/chromedriver")
        .arg("--port=9515")
        .spawn_transparent(&TransparentRunner::new())
        .expect("Failed to start chromedriver");

    // Get host and port from environment or use defaults
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8191".to_string())
        .parse::<u16>()
        .unwrap_or(8191);

    let addr = format!("{host}:{port}");
    println!("FlareSolverr starting on {addr}");

    // Create FlareSolverr API instance
    let api = FlareSolverrAPI::new(FlareSolverrConfig {
        proxy_host,
        proxy_port,
        proxy_username,
        proxy_password,
        scrappey_api_key,
        data_path,
    });
    let app = api.create_router();

    // Create the listener
    let listener = TcpListener::bind(&addr).await?;

    // Start the server
    axum::serve(listener, app).await?;

    // Stop chromedriver when the server stops
    chromedriver.kill().expect("Failed to kill chromedriver");

    Ok(())
}
