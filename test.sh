#!/bin/bash
set -e

echo "========================================="
echo "  build-oci (Rust) - Test Suite"
echo "========================================="
echo ""

PASS=0
FAIL=0

pass() {
    echo "  [PASS] $1"
    PASS=$((PASS + 1))
}

fail() {
    echo "  [FAIL] $1: $2"
    FAIL=$((FAIL + 1))
}

# --------------------------------------------------
# Test 1: Binary exists and runs
# --------------------------------------------------
echo "Test 1: Binary availability"
if command -v build-oci >/dev/null 2>&1; then
    pass "build-oci binary found in PATH"
else
    fail "build-oci binary" "not found in PATH"
fi

# --------------------------------------------------
# Test 2: Basic image build with no layers
# --------------------------------------------------
echo ""
echo "Test 2: Build minimal OCI image (no layers)"

WORKDIR=$(mktemp -d)
cd "$WORKDIR"

cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "test-suite"
    comment: "Minimal test image"
YAML

if [ -f "$WORKDIR/index.json" ]; then
    pass "index.json created"
else
    fail "index.json" "not created"
fi

if [ -f "$WORKDIR/oci-layout" ]; then
    pass "oci-layout created"
else
    fail "oci-layout" "not created"
fi

# Validate index.json structure
SCHEMA_VER=$(jq -r '.schemaVersion' "$WORKDIR/index.json" 2>/dev/null)
if [ "$SCHEMA_VER" = "2" ]; then
    pass "index.json schemaVersion is 2"
else
    fail "index.json schemaVersion" "expected 2, got $SCHEMA_VER"
fi

# Validate oci-layout
LAYOUT_VER=$(jq -r '.imageLayoutVersion' "$WORKDIR/oci-layout" 2>/dev/null)
if [ "$LAYOUT_VER" = "1.0.0" ]; then
    pass "oci-layout imageLayoutVersion is 1.0.0"
else
    fail "oci-layout" "expected 1.0.0, got $LAYOUT_VER"
fi

# Check manifests array
MANIFEST_COUNT=$(jq '.manifests | length' "$WORKDIR/index.json" 2>/dev/null)
if [ "$MANIFEST_COUNT" = "1" ]; then
    pass "index.json contains 1 manifest"
else
    fail "manifest count" "expected 1, got $MANIFEST_COUNT"
fi

# Check blobs directory
if [ -d "$WORKDIR/blobs/sha256" ]; then
    pass "blobs/sha256 directory created"
else
    fail "blobs directory" "not created"
fi

# Validate manifest blob exists and is valid JSON
MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$MANIFEST_HASH" ]; then
    pass "manifest blob exists"
    # Validate manifest content
    MANIFEST_SCHEMA=$(jq -r '.schemaVersion' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
    if [ "$MANIFEST_SCHEMA" = "2" ]; then
        pass "manifest blob is valid JSON with schemaVersion 2"
    else
        fail "manifest blob" "invalid JSON or wrong schemaVersion"
    fi
else
    fail "manifest blob" "file not found: $MANIFEST_HASH"
fi

# Validate config blob exists
CONFIG_DIGEST=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
CONFIG_HASH="${CONFIG_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$CONFIG_HASH" ]; then
    pass "config blob exists"
    # Validate config fields
    CONFIG_ARCH=$(jq -r '.architecture' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    CONFIG_OS=$(jq -r '.os' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    CONFIG_AUTHOR=$(jq -r '.author' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    if [ "$CONFIG_ARCH" = "amd64" ]; then
        pass "config architecture is amd64"
    else
        fail "config architecture" "expected amd64, got $CONFIG_ARCH"
    fi
    if [ "$CONFIG_OS" = "linux" ]; then
        pass "config os is linux"
    else
        fail "config os" "expected linux, got $CONFIG_OS"
    fi
    if [ "$CONFIG_AUTHOR" = "test-suite" ]; then
        pass "config author is test-suite"
    else
        fail "config author" "expected test-suite, got $CONFIG_AUTHOR"
    fi
else
    fail "config blob" "file not found"
fi

# Check platform in manifest descriptor
PLAT_OS=$(jq -r '.manifests[0].platform.os' "$WORKDIR/index.json" 2>/dev/null)
PLAT_ARCH=$(jq -r '.manifests[0].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
if [ "$PLAT_OS" = "linux" ] && [ "$PLAT_ARCH" = "amd64" ]; then
    pass "platform metadata (os=linux, arch=amd64)"
else
    fail "platform metadata" "os=$PLAT_OS, arch=$PLAT_ARCH"
fi

# Check empty_layer in history
EMPTY_LAYER=$(jq -r '.history[0].empty_layer' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$EMPTY_LAYER" = "true" ]; then
    pass "history marks empty_layer=true (no layer specified)"
else
    fail "history empty_layer" "expected true, got $EMPTY_LAYER"
fi

rm -rf "$WORKDIR"

# --------------------------------------------------
# Test 3: Build image with a layer
# --------------------------------------------------
echo ""
echo "Test 3: Build OCI image with a filesystem layer"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)

# Create some test files in the layer
mkdir -p "$LAYER_DIR/usr/bin"
echo '#!/bin/sh' > "$LAYER_DIR/usr/bin/hello"
echo 'echo "Hello from OCI"' >> "$LAYER_DIR/usr/bin/hello"
chmod +x "$LAYER_DIR/usr/bin/hello"
mkdir -p "$LAYER_DIR/etc"
echo "test-container" > "$LAYER_DIR/etc/hostname"

cd "$WORKDIR"

cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "test-suite"
    layer: "$LAYER_DIR"
    config:
      Env:
        - PATH=/usr/bin:/bin
      WorkingDir: /
YAML

if [ -f "$WORKDIR/index.json" ]; then
    pass "index.json created (with layer)"
else
    fail "index.json" "not created (with layer)"
fi

# Check layers in manifest
MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
LAYER_COUNT=$(jq '.layers | length' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_COUNT" = "1" ]; then
    pass "manifest has 1 layer"
else
    fail "layer count" "expected 1, got $LAYER_COUNT"
fi

# Check layer media type
LAYER_MT=$(jq -r '.layers[0].mediaType' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_MT" = "application/vnd.oci.image.layer.v1.tar+gzip" ]; then
    pass "layer mediaType is tar+gzip"
else
    fail "layer mediaType" "expected tar+gzip, got $LAYER_MT"
fi

# Verify layer blob exists and is valid gzip
LAYER_DIGEST=$(jq -r '.layers[0].digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
LAYER_HASH="${LAYER_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$LAYER_HASH" ]; then
    pass "layer blob file exists"
    # Check it's valid gzip
    if gzip -t "$WORKDIR/blobs/sha256/$LAYER_HASH" 2>/dev/null; then
        pass "layer blob is valid gzip"
    else
        fail "layer blob" "not valid gzip"
    fi
else
    fail "layer blob" "file not found"
fi

# Check diff_ids in config
CONFIG_DIGEST=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
CONFIG_HASH="${CONFIG_DIGEST#sha256:}"
DIFF_ID_COUNT=$(jq '.rootfs.diff_ids | length' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$DIFF_ID_COUNT" = "1" ]; then
    pass "config has 1 diff_id"
else
    fail "diff_id count" "expected 1, got $DIFF_ID_COUNT"
fi

# Check config.config fields
CONFIG_ENV=$(jq -r '.config.Env[0]' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$CONFIG_ENV" = "PATH=/usr/bin:/bin" ]; then
    pass "config Env preserved"
else
    fail "config Env" "expected PATH=/usr/bin:/bin, got $CONFIG_ENV"
fi

rm -rf "$WORKDIR" "$LAYER_DIR"

# --------------------------------------------------
# Test 4: Disabled compression
# --------------------------------------------------
echo ""
echo "Test 4: Build with compression disabled"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)
echo "test file" > "$LAYER_DIR/test.txt"

cd "$WORKDIR"

cat <<YAML | build-oci
compression: disabled
images:
  - architecture: arm64
    os: linux
    layer: "$LAYER_DIR"
YAML

MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
LAYER_MT=$(jq -r '.layers[0].mediaType' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_MT" = "application/vnd.oci.image.layer.v1.tar" ]; then
    pass "layer mediaType is tar (no gzip)"
else
    fail "disabled compression" "expected tar, got $LAYER_MT"
fi

rm -rf "$WORKDIR" "$LAYER_DIR"

# --------------------------------------------------
# Test 5: Multiple images
# --------------------------------------------------
echo ""
echo "Test 5: Build multiple images in one index"

WORKDIR=$(mktemp -d)
cd "$WORKDIR"

cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    comment: "amd64 image"
  - architecture: arm64
    os: linux
    comment: "arm64 image"
annotations:
  org.opencontainers.image.description: "Multi-arch test"
YAML

MANIFEST_COUNT=$(jq '.manifests | length' "$WORKDIR/index.json" 2>/dev/null)
if [ "$MANIFEST_COUNT" = "2" ]; then
    pass "index.json contains 2 manifests"
else
    fail "multi-image" "expected 2 manifests, got $MANIFEST_COUNT"
fi

ANNOTATION=$(jq -r '.annotations["org.opencontainers.image.description"]' "$WORKDIR/index.json" 2>/dev/null)
if [ "$ANNOTATION" = "Multi-arch test" ]; then
    pass "index annotations preserved"
else
    fail "annotations" "expected 'Multi-arch test', got '$ANNOTATION'"
fi

# Check each manifest has correct platform
ARCH0=$(jq -r '.manifests[0].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
ARCH1=$(jq -r '.manifests[1].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
if [ "$ARCH0" = "amd64" ] && [ "$ARCH1" = "arm64" ]; then
    pass "multi-arch platforms correct (amd64, arm64)"
else
    fail "multi-arch platforms" "got $ARCH0, $ARCH1"
fi

rm -rf "$WORKDIR"

# --------------------------------------------------
# Test 6: SHA256 digest integrity
# --------------------------------------------------
echo ""
echo "Test 6: Blob digest integrity verification"

WORKDIR=$(mktemp -d)
cd "$WORKDIR"

cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
YAML

ALL_OK=true
for blob_file in "$WORKDIR"/blobs/sha256/*; do
    expected_hash=$(basename "$blob_file")
    actual_hash=$(sha256sum "$blob_file" | cut -d' ' -f1)
    if [ "$expected_hash" != "$actual_hash" ]; then
        fail "digest integrity" "$blob_file: expected $expected_hash, got $actual_hash"
        ALL_OK=false
    fi
done

if [ "$ALL_OK" = true ]; then
    pass "all blob digests match their filenames"
fi

rm -rf "$WORKDIR"

# --------------------------------------------------
# Test 7: SOURCE_DATE_EPOCH support
# --------------------------------------------------
echo ""
echo "Test 7: SOURCE_DATE_EPOCH reproducibility"

WORKDIR1=$(mktemp -d)
WORKDIR2=$(mktemp -d)

cd "$WORKDIR1"
export SOURCE_DATE_EPOCH=1700000000
cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "epoch-test"
YAML

cd "$WORKDIR2"
cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "epoch-test"
YAML

# Compare the two builds - they should be identical
DIGEST1=$(jq -r '.manifests[0].digest' "$WORKDIR1/index.json" 2>/dev/null)
DIGEST2=$(jq -r '.manifests[0].digest' "$WORKDIR2/index.json" 2>/dev/null)

if [ "$DIGEST1" = "$DIGEST2" ] && [ -n "$DIGEST1" ]; then
    pass "SOURCE_DATE_EPOCH produces reproducible builds"
else
    fail "reproducibility" "digests differ: $DIGEST1 vs $DIGEST2"
fi

unset SOURCE_DATE_EPOCH
rm -rf "$WORKDIR1" "$WORKDIR2"

# --------------------------------------------------
# Summary
# --------------------------------------------------
echo ""
echo "========================================="
echo "  Results: $PASS passed, $FAIL failed"
echo "========================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
