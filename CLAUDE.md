# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`scrappey-resolverr-rs` is a Rust, FlareSolverr-compatible HTTP API (`/v1`, `/health`, `/`) that bypasses anti-bot challenges (Cloudflare, DDoS-Guard) by driving a real headful Chrome via `chromedriver` + `xvfb-run` (the `transparent` crate handles the virtual display). When the browser can't solve it, the request falls back to the Scrappey API. A built-in local proxy bridge lets Chrome (which can't do authenticated proxies natively) talk to an authenticated upstream HTTP proxy.

Edition: Rust 2024. Toolchain: nightly (per Dockerfile).

## Common commands

The app is designed to run in Docker — it expects `chromedriver` at `/usr/bin/chromedriver` and writes session/screenshot data under `/data`. Outside Docker you'll need both available locally.

```sh
# Build & run (recommended, includes Xvfb + Chrome + Squid proxy)
docker-compose up --build

# Local build / check / lint (requires nightly + chromedriver locally)
cargo build --release
cargo check
cargo clippy --all-targets
cargo fmt

# Run a single binary locally — all env vars from config.rs::load_from_env must be set
SCRAPPEY_API_KEY=... PROXY_HOST=... PROXY_PORT=... cargo run

# Logging
RUST_LOG=debug cargo run        # verbose
RUST_LOG=info,tracing::span=warn cargo run   # Docker default
```

There are no tests in this repo (`cargo test` runs zero tests).

## Architecture

The request path threads through five layers — knowing the flow makes navigating the code much faster:

```
HTTP /v1 (axum)  →  SessionManager  →  Browser.get()  →  ChallengeHandler  →  [optional] Scrappey
                                              ↓
                                       chromedriver
                                              ↓
                                       127.0.0.1:8080 (fwd_proxy bridge)
                                              ↓
                                       authenticated upstream proxy
```

1. **`main.rs`** is the lifecycle owner. It starts the proxy bridge (background tokio task), spawns `chromedriver` via the `transparent` crate (so signals propagate cleanly), then runs the axum server with graceful shutdown. On shutdown it kills the chromedriver child. Three constants live here that don't come from config: `PROXY_BRIDGE_ADDR` (0.0.0.0:8080), `CHROMEDRIVER_PATH`, `CHROMEDRIVER_PORT`.

2. **`flaresolverr.rs`** is the axum layer. Implements the FlareSolverr v3.3.21 wire format — note that the response shape (`V1Response`, `Solution`, `FlaresolverrCookie`) must stay byte-compatible with FlareSolverr clients like Prowlarr. Handled commands: `request.get`, `request.post` (unimplemented), `sessions.create`, `sessions.list`, `sessions.destroy`. The deprecated v1 params (`headers`, `userAgent`, `download`, `returnRawHtml`) are accepted-and-warned, not honored.

3. **`session.rs`** owns concurrency. `SessionManager` holds `Arc<RwLock<HashMap<String, Session>>>` and a background cleanup task that runs every 60s evicting expired sessions. There is always a pre-loaded `"default"` session (constant `DEFAULT_SESSION_ID`) — it can't be destroyed, and is used when no `session` field is in the request. Per-session data is persisted to `<data_path>/sessions/session_<id>.json`. Holding the write lock blocks all other sessions, so `with_session` should do the minimum needed and clone the `Browser` out for the long-running navigation.

4. **`browser.rs`** wraps thirtyfour. `Browser::get()` creates a fresh `WebDriver` per request (not pooled), restores cookies via Chrome DevTools `Network.setCookie`, navigates, runs challenge handlers, and extracts the response. **Always uses `LOCAL_PROXY_ADDR = 127.0.0.1:8080`** for the browser — never the upstream proxy directly, because chromedriver doesn't support proxy auth. The HTTP status is hardcoded to 200 (`DEFAULT_HTTP_STATUS`) — thirtyfour doesn't expose the real status. `BrowserData` (UA + cookies) is the serialized session state.

5. **`challenge/`** — `ChallengeHandler` trait with two implementations:
   - `ddos_guard.rs` — detects by page title, uses the trait's default polling implementation (1s interval until challenge clears or timeout).
   - `cloudflare.rs` — detects "Just a moment..." title. Uses `handle_with_fallback` which splits the budget: 1/3 of the timeout for browser solving, the remaining 2/3 for Scrappey. If browser solving times out and both Scrappey and proxy are configured, the request is replayed via Scrappey and the resulting cookies/UA are merged back into the session.

6. **`fwd_proxy.rs`** is a hand-rolled HTTP/1.1 proxy that listens on `0.0.0.0:8080` (inside the container) and forwards to the configured upstream proxy, injecting `Proxy-Authorization: Basic <base64>` if credentials are set. Handles both plain HTTP requests and `CONNECT` tunnels.

7. **`scrappey.rs`** wraps the Scrappey publisher API. Used both for the startup balance check (logged at boot) and as the Cloudflare fallback. Cookies returned by Scrappey are converted into `thirtyfour::Cookie` and merged into the browser session.

8. **`config.rs`** — all config comes from env vars at startup; `load_from_env()` is the only public entry point. `ServerConfig::to_browser_config()` is how the API layer builds the `BrowserConfig` passed into sessions. Required env: `SCRAPPEY_API_KEY`, `PROXY_HOST`, `PROXY_PORT`. Everything else has defaults.

9. **`logging.rs`** — structured logging helpers (`LogContext`, `TimingLogger`). `LogContext` chains `.with_url()`, `.with_session()`, `.with_operation()` and emits a single prefixed line. Prefer these helpers over raw `log::info!` when context is available, to keep grep-able output.

## Things to know when editing

- **Wire format is load-bearing.** `V1Request`/`V1Response`/`Solution`/`FlaresolverrCookie` field names (especially the `#[serde(rename)]` and `rename_all = "camelCase"`) match the FlareSolverr API exactly. Breaking that breaks Prowlarr et al.

- **The proxy bridge is mandatory, not optional.** Chrome always points at `127.0.0.1:8080`. If you change `LOCAL_PROXY_ADDR` in `browser.rs`, change `PROXY_BRIDGE_ADDR` in `main.rs` too — they must agree.

- **Every request spawns a fresh `WebDriver`** in `Browser::get()` and unconditionally `quit()`s it (even on error paths). Don't break that — leaked WebDriver instances tie up chromedriver.

- **The default session can't be destroyed** (`session.rs::destroy_session` bails). API responses always include `session: "default"` when no session was supplied — clients use this as the round-trip session identifier.

- **HTTP status is fake.** thirtyfour can't read the real response status, so `extract_response` hardcodes 200. The Scrappey fallback path returns the real status from Scrappey.

- **Cookie expiry handling.** Cookies are filtered for expiry in `Browser::clean_expired_cookies` before being re-injected via CDP. Scrappey cookies come back in a different shape and are converted in `scrappey.rs`.

## Deployment

`.github/workflows/docker-publish.yml` builds and publishes to `ghcr.io/<owner>/scrappey-resolverr-rs` on every push to the `release` branch. There's no separate test workflow. `develop` is the default integration branch.
