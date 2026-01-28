FROM rust:1.83-bookworm AS builder

WORKDIR /build

# Copy source
COPY Cargo.toml Cargo.toml
COPY src/ src/

# Build release binary
RUN cargo build --release 2>&1

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    jq gzip coreutils \
    && rm -rf /var/lib/apt/lists/*

# Copy Rust binary
COPY --from=builder /build/target/release/build-oci /usr/local/bin/build-oci

# Copy test script
COPY test.sh /test.sh
RUN chmod +x /test.sh

WORKDIR /workspace

ENTRYPOINT ["/test.sh"]
