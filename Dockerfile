# ---- Build Stage ----
FROM rustlang/rust:nightly AS builder

WORKDIR /app
COPY . .

# Build in release mode
RUN cargo build --release

# ---- Final Stage ----
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
