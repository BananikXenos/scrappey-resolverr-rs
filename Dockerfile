# ---- Base Stage: Install build tools and caching helpers ----
FROM rustlang/rust:nightly AS base

# Install sccache and cargo-chef for build caching
RUN cargo install sccache --version ^0.7 && \
    cargo install cargo-chef --version ^0.1

ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache

# ---- Planner Stage: Generate dependency recipe ----
FROM base AS planner
WORKDIR /app
COPY . .
# Cache Cargo registry and sccache for dependency resolution
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo chef prepare --recipe-path recipe.json

# ---- Builder Stage: Build dependencies and project ----
FROM base AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
# Cache Cargo registry and sccache for dependency build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
# Cache Cargo registry and sccache for final build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked \
    cargo build --release

# ---- Final Stage: Minimal runtime image ----
FROM archlinux:latest

# Install curl and gnupg for key management
RUN pacman -Sy --noconfirm curl gnupg

# Initialize and populate pacman keyring
RUN pacman-key --init
RUN pacman-key --populate archlinux

# Import Chaotic AUR key
RUN pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com

# Locally sign the Chaotic AUR key
RUN pacman-key --lsign-key 3056513887B78AEB

# Install Chaotic keyring and mirrorlist
RUN pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst'
RUN pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst'

# Add Chaotic AUR to pacman.conf
RUN echo -e '\n[chaotic-aur]\nInclude = /etc/pacman.d/chaotic-mirrorlist' >> /etc/pacman.conf

# Update system and sync mirrors
RUN pacman -Syu --noconfirm

# Install Xvfb, chromedriver, and google-chrome from Chaotic AUR
RUN pacman -S --noconfirm xorg-server-xvfb chromedriver google-chrome

# Clean up package cache
RUN pacman -Scc --noconfirm

# Copy the built binary from the builder stage
COPY --from=builder /app/target/release/scrappey-resolverr-rs /usr/local/bin/scrappey-resolverr-rs

# Expose the port the application listens on
EXPOSE 8191

# Create data directory for persistent storage
RUN mkdir -p /data
VOLUME ["/data"]

# Set environment variables for logging
ENV RUST_LOG=info,tracing::span=warn

# Set default command
CMD ["/usr/local/bin/scrappey-resolverr-rs"]
