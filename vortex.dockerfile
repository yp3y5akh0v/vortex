FROM rust:1.82-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    liburing-dev \
    lld \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /vortex

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock* ./
COPY .cargo .cargo
COPY crates/vortex-io/Cargo.toml crates/vortex-io/
COPY crates/vortex-runtime/Cargo.toml crates/vortex-runtime/
COPY crates/vortex-http/Cargo.toml crates/vortex-http/
COPY crates/vortex-server/Cargo.toml crates/vortex-server/
COPY crates/vortex-json/Cargo.toml crates/vortex-json/
COPY crates/vortex-db/Cargo.toml crates/vortex-db/
COPY crates/vortex-template/Cargo.toml crates/vortex-template/
COPY techempower/Cargo.toml techempower/

# Create dummy source files for dependency compilation
RUN mkdir -p crates/vortex-io/src && echo "" > crates/vortex-io/src/lib.rs && \
    mkdir -p crates/vortex-runtime/src && echo "" > crates/vortex-runtime/src/lib.rs && \
    mkdir -p crates/vortex-http/src && echo "" > crates/vortex-http/src/lib.rs && \
    mkdir -p crates/vortex-server/src && echo "" > crates/vortex-server/src/lib.rs && \
    mkdir -p crates/vortex-json/src && echo "" > crates/vortex-json/src/lib.rs && \
    mkdir -p crates/vortex-db/src && echo "" > crates/vortex-db/src/lib.rs && \
    mkdir -p crates/vortex-template/src && echo "" > crates/vortex-template/src/lib.rs && \
    mkdir -p techempower/src && echo "fn main() {}" > techempower/src/main.rs

# Pre-compile dependencies (cached layer)
RUN cargo build --release 2>/dev/null || true

# Copy actual source code
COPY . .
RUN find crates techempower -name "*.rs" -exec touch {} +

# Build release binary
RUN cargo build --release --bin vortex-bench

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    liburing2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /vortex/target/release/vortex-bench /usr/local/bin/vortex

EXPOSE 8080

CMD ["vortex"]
