# build-oci-rs

A Rust rewrite of the [Freedesktop SDK](https://gitlab.com/nickelfor/nickelfor) OCI image builder, originally written in Python. Builds OCI-compliant container images from YAML configuration provided via stdin.

## Features

- Builds OCI images compliant with the [OCI Image Spec](https://github.com/opencontainers/image-spec)
- Gzip or uncompressed layer support
- Layer deduplication (skips unchanged files)
- Whiteout handling for file deletions (overlay filesystem semantics)
- Extended attribute (xattr) preservation
- Reproducible builds via `SOURCE_DATE_EPOCH`
- Multi-image index output (multi-arch support)
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

### YAML configuration format

```yaml
# Compression: "gzip" (default) or "disabled"
compression: gzip
compression-level: 5          # 1-9, default 5 (only for gzip)

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

The test suite covers:
- Binary availability
- Minimal image build (no layers)
- Image build with filesystem layers
- Disabled compression mode
- Multi-image index builds
- SHA256 blob digest integrity
- SOURCE_DATE_EPOCH reproducibility

## License

MIT - see source file headers for the full license text.
