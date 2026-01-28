# build-oci-rs

A high-performance OCI image builder written in Rust. Builds [OCI-compliant](https://github.com/opencontainers/image-spec) container images from YAML configuration provided via stdin.

Built as a faster replacement for the Python-based builder in the [Freedesktop SDK](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/tree/master/files/oci) pipeline.

## Features

- **Automatic multi-core utilization** — detects available CPU cores and parallelizes file hashing, compression, directory traversal, and multi-image builds
- Parallel image building with configurable worker threads (`-j` / `--workers`)
- **Zstd compression support** — 2-5x faster than gzip at similar compression ratios (OCI-compliant `+zstd` media type)
- Parallel gzip compression via [gzp](https://crates.io/crates/gzp) (pigz-style)
- Multi-threaded zstd compression via [zstd](https://crates.io/crates/zstd)
- **Parallel directory traversal** via [jwalk](https://crates.io/crates/jwalk) — 20-40% faster for large directories
- **Parallel lower layer analysis** — 2-4x faster for builds with 4+ parent layers
- **jemalloc allocator** — 5-15% faster multi-threaded memory allocation
- **FxHashMap** for faster deduplication lookups (10-25% improvement)
- 128 KB buffered I/O for layer packing, hashing, and compression
- Compression with tunable level (gzip: 1-9, zstd: 1-22)
- Layer deduplication (skips unchanged files against parent layers)
- Multi-image index output (multi-arch builds)
- Reproducible builds via `SOURCE_DATE_EPOCH`
- Whiteout handling for overlay filesystem semantics
- Extended attribute (xattr) preservation
- Parent image composition

## Requirements

### Build from source

- Rust 1.70+ and Cargo

### Run in a container (recommended)

- Docker or Podman

## Quick start (container)

```bash
git clone git@github.com:repires/build-oci-rs.git
cd build-oci-rs
docker build -t build-oci .
docker run --rm build-oci        # runs the test suite
```

To use the binary directly from the built image:

```bash
docker run --rm -i -v "$PWD/output:/workspace" build-oci /usr/local/bin/build-oci <<'YAML'
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "my-org"
    layer: /path/to/rootfs
    config:
      Env:
        - PATH=/usr/bin:/bin
      WorkingDir: /
YAML
```

The OCI image output (index.json, oci-layout, blobs/) will be written to `./output/`.

## Platform support

The tool works on both **aarch64** (Apple Silicon, ARM servers) and **x86-64** (Intel/AMD). It automatically detects and uses all available CPU cores on any platform.

**Important**: Docker images are architecture-specific. You must build the image on your own system:

```bash
# On your machine (aarch64 or x86-64)
git clone https://github.com/repires/build-oci-rs.git
cd build-oci-rs
docker build -t build-oci .
```

The resulting binary will be optimized for your CPU architecture and will automatically use all available cores.

## Build from source (without Docker)

```bash
cargo build --release
```

The binary is placed at `target/release/build-oci`.

## Usage

`build-oci` reads a YAML document from **stdin** and writes an OCI image directory to the **current working directory**.

```bash
cat config.yaml | build-oci
```

### CLI options

| Flag | Description |
|------|-------------|
| `-j N` / `--workers N` | Number of parallel worker threads (default: number of CPU cores) |

```bash
# Build using 4 parallel workers
cat config.yaml | build-oci -j 4

# Build single-threaded
cat config.yaml | build-oci -j 1
```

### YAML configuration format

```yaml
# Compression: "zstd" (default, fastest), "gzip", or "disabled"
compression: zstd
compression-level: 3          # zstd: 1-22 (default 3), gzip: 1-9 (default 5)

# Performance tuning (optional)
skip-xattrs: false            # Skip xattr handling for faster builds (default: false)
prefetch-limit-mb: 512        # Memory limit for file prefetch cache in MB (default: 512)

# Optional top-level annotations added to the OCI index
annotations:
  org.opencontainers.image.description: "My container image"

images:
  - architecture: amd64       # required
    os: linux                  # required
    author: "My Name"          # optional
    comment: "Build info"      # optional
    variant: "v8"              # optional (for ARM variants, etc.)

    # Filesystem directory to pack as a layer
    layer: /path/to/rootfs

    # Optional parent image to extend
    parent:
      image: /path/to/parent-oci-dir
      index: 0                 # manifest index in parent (default 0)

    # OCI image config (passed through as-is)
    config:
      Env:
        - PATH=/usr/bin:/bin
      WorkingDir: /
      Cmd:
        - /bin/sh

    # Annotations on the manifest itself
    annotations:
      org.opencontainers.image.title: "my-image"

    # Annotations on the index entry for this manifest
    index-annotations:
      org.opencontainers.image.ref.name: "latest"
```

### Multi-architecture example

```yaml
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: /build/rootfs-amd64
  - architecture: arm64
    os: linux
    layer: /build/rootfs-arm64
```

### Zstd compression (faster builds)

Zstd compression is 2-5x faster than gzip while achieving similar or better compression ratios. It's fully OCI-compliant and supported by modern container runtimes.

```yaml
compression: zstd
compression-level: 3          # 1-22, default 3 (good balance of speed/ratio)
images:
  - architecture: amd64
    os: linux
    layer: /build/rootfs
```

**Compression level guidelines:**
- Level 1-3: Fast compression, good for CI/CD pipelines
- Level 6-9: Balanced compression (similar to gzip level 5-6)
- Level 19-22: Maximum compression (slower, for distribution)

### Reproducible builds

Set `SOURCE_DATE_EPOCH` to get deterministic timestamps and reproducible output:

```bash
export SOURCE_DATE_EPOCH=$(date +%s)
cat config.yaml | build-oci
```

## Output structure

```
./
├── index.json          # OCI image index
├── oci-layout          # OCI layout descriptor
└── blobs/
    └── sha256/
        ├── <config>    # Image config JSON
        ├── <manifest>  # Image manifest JSON
        └── <layer>     # Layer tar (or tar+gzip)
```

## Running tests

### In a container

```bash
docker build -t build-oci .
docker run --rm build-oci
```

The test suite (78 assertions across 14 tests) covers:

- Binary availability and error handling
- Minimal image build (no layers)
- Image build with filesystem layers
- Disabled compression mode
- Multi-image index builds
- SHA256 blob digest integrity
- `SOURCE_DATE_EPOCH` reproducibility
- Stress test (500 files, ~2 MB layer)
- File permissions and ownership preservation
- OCI annotation propagation
- Workers flag (`-j`, `--workers`, `-jN`)
- Optional: comparison against the original Python builder (structural equivalence + performance benchmark)

## Performance

### Compression comparison (100MB layer)

| Compression | Time | Ratio |
|-------------|------|-------|
| Gzip (level 5) | ~2.6s | ~35% |
| Zstd (level 3) | ~0.8s | ~33% |
| Disabled | ~0.3s | 100% |

Zstd is **2-3x faster** than gzip at similar compression ratios.

### Parallelization benchmarks (50MB layer, 500 files)

| Workers | Time |
|---------|------|
| 1 core | 2762ms |
| All cores | 778ms |

Parallel speedup: **~3.5x** on multi-core systems.

### Optimization impact

| Optimization | Speedup |
|--------------|---------|
| jemalloc allocator | 5-15% |
| jwalk parallel traversal | 20-40% (large directories) |
| FxHashMap deduplication | 10-25% |
| Parallel lower analysis | 2-4x (4+ parent layers) |
| Zstd vs gzip | 2-5x compression speed |
| skip-xattrs: true | 10-30% (eliminates syscalls) |
| Memory-bounded prefetch | Prevents OOM on large dirs |

Compared to the original Python implementation: **~3x faster** on the test suite benchmark.

## Python reference

The `python-original/` directory contains the original Python implementation from the [Freedesktop SDK OCI builder](https://gitlab.com/freedesktop-sdk/freedesktop-sdk/-/tree/master/files/oci). It is included for anyone who wants to inspect or compare the two implementations. The test suite optionally runs comparison tests when the Python builder is available in the container.

## License

MIT - see source file headers for the full license text.
