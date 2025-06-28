use anyhow::Result;
use tokio::net::TcpListener;
mod browser;
mod challenge;
mod flaresolverr;
mod fwd_proxy;
mod scrappey;
use flaresolverr::{FlareSolverrAPI, FlareSolverrConfig};

use crate::fwd_proxy::{HttpProxyBridge, ProxyConfig};

const FLARESOLVERR_VERSION: &str = "3.3.21";

fn extract_environment_variables() -> Result<FlareSolverrConfig> {
    // Extract environment variables with defaults
    let api_key = std::env::var("SCRAPPEY_API_KEY")?;
    let proxy_host = std::env::var("PROXY_HOST")?;
    let proxy_port = std::env::var("PROXY_PORT")?
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid PROXY_PORT"))?;
    let proxy_username = std::env::var("PROXY_USERNAME").ok();
    let proxy_password = std::env::var("PROXY_PASSWORD").ok();

    Ok(FlareSolverrConfig {
        proxy_host,
        proxy_port,
        proxy_username,
        proxy_password,
        scrappey_api_key: api_key,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Extract environment variables
    let config = extract_environment_variables()?;

    // Run local http to socks5 proxy server
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

    let mut bridge = HttpProxyBridge::new(proxy_config);
    bridge.bind("0.0.0.0:8080".parse()?).await?;
    tokio::spawn(async move {
        if let Err(e) = bridge.serve().await {
            eprintln!("Error running proxy bridge: {}", e);
        }
    });

    // Get host and port from environment or use defaults
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8191".to_string())
        .parse::<u16>()
        .unwrap_or(8191);

    let addr = format!("{}:{}", host, port);
    println!("FlareSolverr {} starting on {}", FLARESOLVERR_VERSION, addr);

    // Create FlareSolverr API instance
    let api = FlareSolverrAPI::new(config);
    let app = api.create_router();

    // Create the listener
    let listener = TcpListener::bind(&addr).await?;

    // Start the server
    axum::serve(listener, app).await?;

    Ok(())
}
