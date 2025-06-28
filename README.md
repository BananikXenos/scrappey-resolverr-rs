# scrappey-resolverr-rs 🚀🦀

A high-performance, Rust-based, FlareSolverr-compatible API for bypassing anti-bot challenges (Cloudflare, DDoS-Guard, etc.) using a headful Chrome browser running inside a virtual display (via `transparent` and `xvfb-run`), with Scrappey fallback and built-in authenticated HTTP proxy bridging.

---

## Overview 📖

**scrappey-resolverr-rs** is a modern, Docker-ready replacement for [FlareSolverr](https://github.com/FlareSolverr/FlareSolverr), written in Rust for speed and reliability. It exposes a FlareSolverr-compatible HTTP API, orchestrates a headful Chrome browser running inside a virtual display (using the `transparent` library and `xvfb-run`) to solve anti-bot challenges, and can fall back to the Scrappey API for advanced bypassing. It also includes a local HTTP-to-authenticated-HTTP proxy bridge, making it easy to use authenticated proxies with browser automation.

---

## How it Works ⚙️

1. **API Requests:**
   The server exposes endpoints compatible with FlareSolverr (`/v1`, `/health`, `/`).

2. **Challenge Handling:**
   - Receives a request to fetch a URL.
   - Launches a headful Chrome session (not headless) via `chromedriver`, running inside a virtual display using the `transparent` library and `xvfb-run`.
   - Navigates to the target URL, handling cookies and user-agent spoofing.
   - Detects and solves anti-bot challenges (Cloudflare, DDoS-Guard) automatically.
   - If browser-based solving fails, falls back to the [Scrappey](https://scrappey.com/) API.

3. **Proxy Bridge:**
   - Runs a local HTTP proxy on port `8080` that forwards requests to an upstream authenticated HTTP proxy (as configured in Docker).
   - Chrome is configured to use this bridge, enabling authenticated proxy support.

4. **Persistence:**
   - Cookies and user-agent are persisted to disk (`/data/persistent.json`) for session continuity.

---

## Architecture 🏗️

- **Rust (tokio, axum):** High-performance async server and proxy.
- **Headful Chrome (chromedriver + transparent + xvfb-run):** Real browser automation for challenge solving, running in a virtual display (not headless).
- **Scrappey API:** Fallback for advanced anti-bot bypass.
- **HTTP Proxy Bridge:** Local proxy for authenticated upstream proxies.
- **Dockerized:** All dependencies (Chrome, chromedriver, proxy) managed via Docker Compose.

**Key Components:**
- `src/main.rs` — Entrypoint, config, server, and process management.
- `src/flaresolverr.rs` — FlareSolverr-compatible API handlers.
- `src/browser.rs` — Browser automation and challenge logic.
- `src/challenge.rs` — Challenge detection and solving.
- `src/fwd_proxy.rs` — HTTP proxy bridge implementation.
- `src/scrappey.rs` — Scrappey API client.

---

## Installation 🐳

### Prerequisites 📦

- [Docker](https://www.docker.com/)
- [Docker Compose](https://docs.docker.com/compose/)

### Quick Start 🚦

#### Option 1: Use Prebuilt Docker Image (Recommended)

You can use the prebuilt image from GitHub Container Registry without building locally:

1. **Configure environment variables:**
   Edit `docker-compose.yml` and set:
   - `SCRAPPEY_API_KEY` (get from [Scrappey](https://scrappey.com/))
   - `PROXY_HOST`, `PROXY_PORT`, `PROXY_USERNAME`, `PROXY_PASSWORD` (your HTTP proxy credentials)

2. **Update your `docker-compose.yml`:**
   In the `scrappey-resolverr` service section, set the image to:
   ```yaml
   image: ghcr.io/bananikxenos/scrappey-resolverr-rs:release
   ```
   Remove or comment out any `build:` lines for this service.

3. **Start the services:**
   ```sh
   docker-compose up -d
   ```

   This will:
   - Start an authenticated Squid proxy (`proxy` service)
   - Pull and run the prebuilt `scrappey-resolverr-rs` image (`scrappey-resolverr` service)
   - Launch Chrome and chromedriver inside the container

4. **API will be available at:**
   `http://localhost:8191` 🎯

---

#### Option 2: Build Locally

1. **Clone the repository:**
   ```sh
   git clone <this-repo-url>
   cd scrappey-resolverr-rs
   ```

2. **Configure environment variables:**
   Edit `docker-compose.yml` and set:
   - `SCRAPPEY_API_KEY` (get from [Scrappey](https://scrappey.com/))
   - `PROXY_HOST`, `PROXY_PORT`, `PROXY_USERNAME`, `PROXY_PASSWORD` (your HTTP proxy credentials)

3. **Start the services:**
   ```sh
   docker-compose up --build
   ```

   This will:
   - Start an authenticated Squid proxy (`proxy` service)
   - Build and run `scrappey-resolverr-rs` (`scrappey-resolverr` service)
   - Launch Chrome and chromedriver inside the container

4. **API will be available at:**
   `http://localhost:8191` 🎯

---

## Usage Examples 🧑‍💻

### Health Check ❤️

```sh
curl http://localhost:8191/health
```

### Solve a Challenge (GET request) 🛡️

```sh
curl -X POST http://localhost:8191/v1 \
  -H 'Content-Type: application/json' \
  -d '{
    "cmd": "request.get",
    "url": "https://protected-site.com/",
    "maxTimeout": 60000
  }'
```

### Example Response 📦

```json
{
  "status": "ok",
  "message": "Challenge solved!",
  "solution": {
    "url": "https://protected-site.com/",
    "status": 200,
    "headers": {},
    "response": "<html>...</html>",
    "cookies": [
      {
        "name": "...",
        "value": "...",
        "domain": "...",
        "path": "/",
        "expires": 1712345678,
        "httpOnly": false,
        "secure": true,
        "sameSite": "Lax"
      }
    ],
    "userAgent": "Mozilla/5.0 ..."
  }
}
```

---

## Notes

- **Persistence:** Cookies and user-agent are saved in `/data/persistent.json` (mounted as a Docker volume).
- **Proxy:** Chrome always connects to the local proxy bridge (`127.0.0.1:8080`), which forwards to your configured authenticated proxy.
- **Fallback:** If browser-based solving fails, Scrappey API is used (requires a valid API key and balance).
- **Sessions:** Session management is not implemented (stateless per request).

---

## Prowlarr Configuration 🦁🔧

To use scrappey-resolverr-rs with [Prowlarr](https://github.com/Prowlarr/Prowlarr):

### 1. Go to Settings → Indexers ⚙️

### 2. Add Two Proxies 🧩

**FlareSolverr Proxy:** 🌩️
- **Host:** Locally connectable IP of your scrappey-resolverr instance (e.g., the LAN IP or Docker network IP accessible from your Prowlarr host)
- **Port:** 8191 (default FlareSolverr port)
- **Tags:** a tag like `scrappey`

**HTTP Proxy:** 🌐
- **Host:** Your **publicly exposed** proxy address (the proxy must be accessible from the public internet, as Scrappey will use it externally to act on your IP)
- **Port:** Your `PROXY_PORT`
- **Username:** Your `PROXY_USERNAME`
- **Password:** Your `PROXY_PASSWORD`
- **Tags:** a tag like `proxy`

### 3. For Each Indexer That Needs Cloudflare Bypass 🛡️

- Edit the indexer settings
- Add both tags you created to the "Tags" field

This ensures that requests for those indexers are routed through both the FlareSolverr-compatible API and your authenticated HTTP proxy.

**Why is the HTTP proxy required?** 🤔

The HTTP proxy is essential for maintaining **IP persistence**. This means that cookies and sessions remain valid across requests, as all browser and API traffic is routed through the same outgoing IP. 🍪🔒 As a result, cookies and user-agents do **not** need to be refreshed on every call, which dramatically reduces the number of Scrappey API calls required. This leads to more stable scraping sessions and significant savings on Scrappey usage. 💸✨

---

## License 📄

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
