# ---- Base Stage: Install build tools and caching helpers ----
# Pinned to nightly because the `transparent` crate's build.rs has a top-level
# `#![feature(exit_status_error)]` attribute. The feature is only *used* on
# Windows but the attribute is at crate level, so stable Rust refuses to compile
# the build script on any platform. Track upstream for a fix:
#   https://github.com/OpenByteDev/transparent
# (Edition 2024 has been stable since 1.85; let-chains since 1.88; both fine.)
FROM rustlang/rust:nightly AS base

# cargo-chef alone gets us recipe-based dep caching. sccache used to be
# here too but its 0.7 line stopped compiling on current nightly (opendal
# API drift); not worth chasing for the small wrapper-cache win when
# BuildKit cache mounts already cache the cargo registry and target dir.
RUN cargo install cargo-chef --version ^0.1

# ---- Planner Stage: Generate dependency recipe ----
FROM base AS planner
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef prepare --recipe-path recipe.json

# ---- Builder Stage: Build dependencies and project ----
# cargo-chef's caching model writes compiled deps into /app/target as part
# of the layer (not a cache mount), so the subsequent `cargo build` can
# see them without re-downloading. Layer caching alone keeps recipe.json-
# unchanged builds fast across runs. Only the registry uses a cache mount.
FROM base AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

# ---- Final Stage: Debian-based runtime ----
# Replaces the previous Arch + Chaotic AUR runtime, which lacked an
# `linux/arm64` manifest and depended on a rolling-release AUR for
# both Chrome and chromedriver (i.e. silently drifting versions). Now
# we pin Chrome + chromedriver to a known-compatible pair from the
# Chrome-for-Testing infrastructure. amd64 only — Chrome-for-Testing
# doesn't ship `linux-arm64`, so building on an ARM host requires
# `--platform=linux/amd64` (emulation under OrbStack/Rosetta).
#
# `debian:trixie-slim` matches the glibc (2.41) of the `rustlang/rust:nightly`
# build stage — bookworm's 2.36 was too old, so the linked binary failed
# at startup with "GLIBC_2.39 not found".
FROM debian:trixie-slim AS runtime

# Pin Chrome (and its matching chromedriver) here. Bump in lockstep.
ARG CHROME_VERSION=149.0.7827.22

ENV DEBIAN_FRONTEND=noninteractive

# Chrome runtime dependencies + xvfb for the transparent virtual display +
# curl for the HEALTHCHECK and the install steps below. Several libs got
# the `t64` suffix in Debian trixie (64-bit time_t migration); using the
# old bookworm names here would fail at apt time.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      unzip \
      xvfb \
      xauth \
      dumb-init \
      fonts-liberation \
      libasound2t64 \
      libatk-bridge2.0-0t64 \
      libatk1.0-0t64 \
      libcairo2 \
      libcups2t64 \
      libdbus-1-3 \
      libdrm2 \
      libgbm1 \
      libgtk-3-0t64 \
      libnspr4 \
      libnss3 \
      libpango-1.0-0 \
      libx11-6 \
      libxcb1 \
      libxcomposite1 \
      libxdamage1 \
      libxext6 \
      libxfixes3 \
      libxkbcommon0 \
      libxrandr2 \
      libxss1 \
      && rm -rf /var/lib/apt/lists/*

# Pull pinned Chrome + chromedriver. Both come from the same Chrome-for-Testing
# build so they're guaranteed to match — the historical "chrome 117 vs
# chromedriver 116" drift class is impossible.
RUN curl -fsSL -o /tmp/chrome.zip \
      "https://storage.googleapis.com/chrome-for-testing-public/${CHROME_VERSION}/linux64/chrome-linux64.zip" \
 && curl -fsSL -o /tmp/chromedriver.zip \
      "https://storage.googleapis.com/chrome-for-testing-public/${CHROME_VERSION}/linux64/chromedriver-linux64.zip" \
 && unzip -q /tmp/chrome.zip -d /opt \
 && unzip -q /tmp/chromedriver.zip -d /opt \
 && ln -s /opt/chrome-linux64/chrome /usr/bin/google-chrome \
 && ln -s /opt/chromedriver-linux64/chromedriver /usr/bin/chromedriver \
 && rm /tmp/chrome.zip /tmp/chromedriver.zip

# Copy the built binary from the builder stage
COPY --from=builder /app/target/release/scrappey-resolverr-rs /usr/local/bin/scrappey-resolverr-rs

EXPOSE 8191

# Persistent data (session cookies, failure screenshots)
RUN mkdir -p /data
VOLUME ["/data"]

ENV RUST_LOG=info,tracing::span=warn

# Real liveness signal — checks both the API process and chromedriver via /health.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8191/health > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["/usr/local/bin/scrappey-resolverr-rs"]
