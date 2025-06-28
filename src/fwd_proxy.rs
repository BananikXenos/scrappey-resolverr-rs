#![allow(dead_code)]

//! HTTP-to-HTTP proxy bridge for forwarding requests to an upstream proxy,
//! with optional authentication support. Used to bridge no-auth local proxy
//! to authenticated upstream proxies for browser automation.

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Configuration for the HTTP-to-HTTP proxy bridge.
/// Allows specifying upstream proxy address, port, and optional authentication.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Downstream HTTP proxy server address
    pub http_proxy_addr: String,
    /// Downstream HTTP proxy server port
    pub http_proxy_port: u16,
    /// Optional username for downstream proxy authentication
    pub username: Option<String>,
    /// Optional password for downstream proxy authentication
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Create a new proxy configuration without authentication.
    pub fn new(http_proxy_addr: String, http_proxy_port: u16) -> Self {
        Self {
            http_proxy_addr,
            http_proxy_port,
            username: None,
            password: None,
        }
    }

    /// Create a new proxy configuration with username/password authentication.
    pub fn with_auth(
        http_proxy_addr: String,
        http_proxy_port: u16,
        username: String,
        password: String,
    ) -> Self {
        Self {
            http_proxy_addr,
            http_proxy_port,
            username: Some(username),
            password: Some(password),
        }
    }
}

/// HTTP-to-HTTP proxy bridge server.
/// Listens for local connections and forwards them to the configured upstream proxy.
pub struct HttpProxyBridge {
    config: Arc<ProxyConfig>,
    listener: Option<TcpListener>,
}

impl HttpProxyBridge {
    /// Create a new proxy bridge with the given configuration.
    pub fn new(config: ProxyConfig) -> Self {
        Self {
            config: Arc::new(config),
            listener: None,
        }
    }

    /// Bind the proxy server to the specified local address.
    pub async fn bind(&mut self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        log::info!("HTTP proxy bridge bound to {addr}");
        log::info!(
            "Forwarding to HTTP proxy at {}:{}",
            self.config.http_proxy_addr,
            self.config.http_proxy_port
        );
        self.listener = Some(listener);
        Ok(())
    }

    /// Start the proxy server (runs indefinitely, handling incoming connections).
    pub async fn serve(&self) -> Result<()> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| anyhow!("Server not bound. Call bind() first."))?;

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let config = Arc::clone(&self.config);
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, addr, config).await {
                            log::error!("Error handling client {addr}: {e}");
                        }
                    });
                }
                Err(e) => {
                    log::error!("Failed to accept connection: {e}");
                }
            }
        }
    }

    /// Get the local address the server is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener
            .as_ref()
            .ok_or_else(|| anyhow!("Server not bound"))?
            .local_addr()
            .map_err(Into::into)
    }
}

/// Convenience function to create and run a proxy bridge server.
/// Binds and serves on the given address.
pub async fn run_http_proxy_bridge(bind_addr: SocketAddr, config: ProxyConfig) -> Result<()> {
    let mut bridge = HttpProxyBridge::new(config);
    bridge.bind(bind_addr).await?;
    bridge.serve().await
}

/// Handle a single client connection.
/// Determines if the request is a CONNECT tunnel or a regular HTTP request.
async fn handle_client(
    client_stream: TcpStream,
    client_addr: SocketAddr,
    config: Arc<ProxyConfig>,
) -> Result<()> {
    log::info!("New client connection from {client_addr}");

    let mut reader = BufReader::new(client_stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await? == 0 {
        // Empty request, possibly from a port scanner
        return Ok(());
    }

    if request_line.trim().is_empty() {
        return Ok(());
    }

    log::debug!("Request line: {}", request_line.trim());

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(anyhow!("Invalid HTTP request line"));
    }

    let method = parts[0];
    let url = parts[1];

    match method {
        "CONNECT" => handle_connect_method(reader, url, config).await,
        _ => handle_regular_method(reader, &request_line, config).await,
    }
}

/// Handle an HTTP CONNECT request (for HTTPS tunneling).
/// Establishes a tunnel through the upstream proxy and forwards data bidirectionally.
async fn handle_connect_method(
    client_reader: BufReader<TcpStream>,
    target: &str,
    config: Arc<ProxyConfig>,
) -> Result<()> {
    log::info!("Handling CONNECT to {target}");

    // Connect to the downstream HTTP proxy
    let mut proxy_stream = connect_to_downstream_proxy(&config).await?;

    // --- Send CONNECT request to the downstream proxy ---
    let mut connect_request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");

    // Add authentication header if configured
    if let (Some(username), Some(password)) = (&config.username, &config.password) {
        let credentials = format!("{username}:{password}");
        let encoded = general_purpose::STANDARD.encode(credentials);
        connect_request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }

    connect_request.push_str("Connection: close\r\n\r\n"); // End of headers
    proxy_stream.write_all(connect_request.as_bytes()).await?;

    // --- Read response from the downstream proxy ---
    let mut proxy_reader = BufReader::new(&mut proxy_stream);
    let mut response_line = String::new();
    proxy_reader.read_line(&mut response_line).await?;

    if !response_line.contains("200") {
        // Forward the error response to the client and close
        let mut full_response = response_line.clone();
        loop {
            response_line.clear();
            if proxy_reader.read_line(&mut response_line).await? == 0 || response_line == "\r\n" {
                break;
            }
            full_response.push_str(&response_line);
        }
        let mut client_stream = client_reader.into_inner();
        client_stream.write_all(full_response.as_bytes()).await?;
        log::warn!("Downstream proxy denied CONNECT: {}", full_response.trim());
        return Err(anyhow!(
            "Downstream proxy denied CONNECT: {}",
            full_response.trim()
        ));
    }

    // We got a 200, so the tunnel is established.
    // Discard the remaining headers from the downstream proxy's response.
    let mut line = String::new();
    loop {
        line.clear();
        proxy_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // Now, send the "200 Connection established" back to the original client
    let mut client_stream = client_reader.into_inner();
    client_stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;

    // Read and discard any remaining headers from the original client's CONNECT request
    let mut client_buf_reader = BufReader::new(&mut client_stream);
    loop {
        line.clear();
        client_buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // Start bidirectional forwarding
    forward_streams(client_stream, proxy_stream).await
}

/// Handle a regular HTTP request (not CONNECT).
/// Forwards the request and headers to the upstream proxy, adds authentication if needed,
/// and then forwards data bidirectionally.
async fn handle_regular_method(
    mut client_reader: BufReader<TcpStream>,
    request_line: &str,
    config: Arc<ProxyConfig>,
) -> Result<()> {
    log::info!("Handling regular request: {}", request_line.trim());

    // Connect to the downstream HTTP proxy
    let mut proxy_stream = connect_to_downstream_proxy(&config).await?;

    // Forward the initial request line
    proxy_stream.write_all(request_line.as_bytes()).await?;

    // Add Proxy-Authorization header if needed, then forward the rest of the headers
    let mut request_headers = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        client_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        request_headers.push(line.clone());
    }

    if let (Some(username), Some(password)) = (&config.username, &config.password) {
        let credentials = format!("{username}:{password}");
        let encoded = general_purpose::STANDARD.encode(credentials);
        let auth_header = format!("Proxy-Authorization: Basic {encoded}\r\n");
        proxy_stream.write_all(auth_header.as_bytes()).await?;
    }

    for header in request_headers {
        proxy_stream.write_all(header.as_bytes()).await?;
    }
    // End of headers
    proxy_stream.write_all(b"\r\n").await?;

    // Start bidirectional forwarding for the request body (if any) and the response
    let client_stream = client_reader.into_inner();
    forward_streams(client_stream, proxy_stream).await
}

/// Forward data bidirectionally between two streams (client <-> proxy).
/// Used for both CONNECT tunnels and regular HTTP requests.
async fn forward_streams(mut client_stream: TcpStream, mut proxy_stream: TcpStream) -> Result<()> {
    match tokio::io::copy_bidirectional(&mut client_stream, &mut proxy_stream).await {
        Ok((_client_to_proxy, _proxy_to_client)) => Ok(()),
        Err(e) => {
            log::warn!("Bidirectional forwarding ended with error: {e}");
            Err(e.into())
        }
    }
}

/// Establish a raw TCP connection to the downstream proxy.
/// Resolves the address and connects asynchronously.
async fn connect_to_downstream_proxy(config: &ProxyConfig) -> Result<TcpStream> {
    let addr = format!("{}:{}", config.http_proxy_addr, config.http_proxy_port);
    let mut proxy_addrs = addr.to_socket_addrs()?;

    let proxy_addr = proxy_addrs
        .next()
        .ok_or_else(|| anyhow!("Failed to resolve downstream proxy address"))?;

    let stream = TcpStream::connect(proxy_addr).await?;
    Ok(stream)
}
