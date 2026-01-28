FROM rust:1.83-bookworm AS builder

# Install cmake for libz-sys (required by gzp)
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy source
COPY Cargo.toml Cargo.toml
COPY src/ src/

# Build release binary
RUN cargo build --release 2>&1

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    jq gzip coreutils python3 python3-yaml tar file diffutils time \
    && rm -rf /var/lib/apt/lists/*

# Copy Rust binary
COPY --from=builder /build/target/release/build-oci /usr/local/bin/build-oci

# Copy original Python code and install it
COPY python-original/ /opt/python-oci/
RUN cd /opt/python-oci && python3 setup.py install 2>/dev/null || \
    (cd /opt/python-oci && python3 -m pip install --break-system-packages . 2>/dev/null) || \
    (echo '#!/usr/bin/env python3' > /usr/local/bin/build-oci-py && \
     echo 'import sys; sys.path.insert(0, "/opt/python-oci")' >> /usr/local/bin/build-oci-py && \
     echo 'from oci_builder.cmd import main; main()' >> /usr/local/bin/build-oci-py && \
     chmod +x /usr/local/bin/build-oci-py)

# Copy test script
COPY test.sh /test.sh
RUN chmod +x /test.sh

WORKDIR /workspace

ENTRYPOINT ["/test.sh"]
