FROM rust:1.83-bookworm AS builder

# Install cmake for libz-sys (required by gzp)
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*

# Install llvm-tools component for PGO (provides llvm-profdata that matches rustc's LLVM)
RUN rustup component add llvm-tools-preview

WORKDIR /build

# Copy source
COPY Cargo.toml Cargo.toml
COPY src/ src/

# ============================================================
# Profile-Guided Optimization (PGO) Build
# ============================================================
# Step 1: Build with profiling instrumentation
ENV LLVM_PROFILE_FILE="/build/pgo-data/default_%m_%p.profraw"
RUN RUSTFLAGS="-Cprofile-generate=/build/pgo-data -Ctarget-cpu=native" \
    cargo build --release 2>&1

# Step 2: Generate training data by running a representative workload
RUN mkdir -p /tmp/pgo-train/layer && \
    for i in $(seq 1 500); do \
        mkdir -p /tmp/pgo-train/layer/dir$i && \
        echo "file content $i" > /tmp/pgo-train/layer/dir$i/file.txt; \
    done && \
    echo 'images: [{architecture: amd64, os: linux, layer: /tmp/pgo-train/layer}]' | \
    /build/target/release/build-oci && \
    rm -rf /tmp/pgo-train index.json oci-layout blobs

# Step 3: Merge profile data using Rust's bundled llvm-profdata
# Find the correct llvm-profdata from rustup's llvm-tools
RUN LLVM_PROFDATA=$(find $(rustc --print sysroot) -name 'llvm-profdata' -type f | head -1) && \
    echo "Using: $LLVM_PROFDATA" && \
    $LLVM_PROFDATA merge -o /build/pgo-data/merged.profdata /build/pgo-data/*.profraw

# Step 4: Rebuild with profile data and native CPU optimizations
RUN cargo clean && \
    RUSTFLAGS="-Cprofile-use=/build/pgo-data/merged.profdata -Ctarget-cpu=native" \
    cargo build --release 2>&1

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    jq gzip coreutils python3 python3-yaml tar file diffutils time \
    && rm -rf /var/lib/apt/lists/*

# Copy PGO-optimized Rust binary
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

# Copy benchmark script
COPY benchmark.sh /benchmark.sh
RUN chmod +x /benchmark.sh

WORKDIR /workspace

ENTRYPOINT ["/test.sh"]
