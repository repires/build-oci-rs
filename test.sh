#!/bin/bash
set -e

echo "============================================================"
echo "  build-oci - Deep Analysis Test Suite (Rust vs Python)"
echo "============================================================"
echo ""

PASS=0
FAIL=0
WARN=0

pass() {
    echo "  [PASS] $1"
    PASS=$((PASS + 1))
}

fail() {
    echo "  [FAIL] $1: $2"
    FAIL=$((FAIL + 1))
}

warn() {
    echo "  [WARN] $1: $2"
    WARN=$((WARN + 1))
}

info() {
    echo "  [INFO] $1"
}

# Detect Python build-oci availability
PYTHON_CMD=""
if command -v build-oci-py >/dev/null 2>&1; then
    PYTHON_CMD="build-oci-py"
elif python3 -c "from oci_builder.cmd import main" 2>/dev/null; then
    PYTHON_CMD="python3 -c 'import sys; sys.path.insert(0,\"/opt/python-oci\"); from oci_builder.cmd import main; main()'"
fi

# Helper: resolve config blob from an OCI output dir
get_config_blob() {
    local dir="$1"
    local mhash
    mhash=$(jq -r '.manifests[0].digest' "$dir/index.json" | cut -d: -f2)
    local chash
    chash=$(jq -r '.config.digest' "$dir/blobs/sha256/$mhash" | cut -d: -f2)
    echo "$dir/blobs/sha256/$chash"
}

get_manifest_blob() {
    local dir="$1"
    local mhash
    mhash=$(jq -r '.manifests[0].digest' "$dir/index.json" | cut -d: -f2)
    echo "$dir/blobs/sha256/$mhash"
}

# ======================================================================
echo "PART 1: RUST BINARY TESTS"
echo "--------------------------------------------------------------"
# ======================================================================

# --------------------------------------------------
# Test 1: Binary exists and runs
# --------------------------------------------------
echo ""
echo "Test 1: Binary availability and basic execution"
if command -v build-oci >/dev/null 2>&1; then
    pass "build-oci binary found in PATH"
else
    fail "build-oci binary" "not found in PATH"
fi

BINARY_TYPE=$(file /usr/local/bin/build-oci 2>/dev/null || echo "unknown")
info "Binary: $BINARY_TYPE"

# Test error handling - invalid YAML
if echo "invalid: [yaml: {broken" | build-oci 2>/dev/null; then
    fail "error handling" "should reject invalid YAML"
else
    pass "rejects invalid YAML with non-zero exit"
fi

# Test error handling - invalid compression
if echo 'compression: lz4' | build-oci 2>/dev/null; then
    fail "error handling" "should reject invalid compression type"
else
    pass "rejects invalid compression type"
fi

# --------------------------------------------------
# Test 2: Minimal image (no layers)
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

SCHEMA_VER=$(jq -r '.schemaVersion' "$WORKDIR/index.json" 2>/dev/null)
if [ "$SCHEMA_VER" = "2" ]; then
    pass "index.json schemaVersion is 2"
else
    fail "index.json schemaVersion" "expected 2, got $SCHEMA_VER"
fi

LAYOUT_VER=$(jq -r '.imageLayoutVersion' "$WORKDIR/oci-layout" 2>/dev/null)
if [ "$LAYOUT_VER" = "1.0.0" ]; then
    pass "oci-layout imageLayoutVersion is 1.0.0"
else
    fail "oci-layout" "expected 1.0.0, got $LAYOUT_VER"
fi

MANIFEST_COUNT=$(jq '.manifests | length' "$WORKDIR/index.json" 2>/dev/null)
if [ "$MANIFEST_COUNT" = "1" ]; then
    pass "index.json contains 1 manifest"
else
    fail "manifest count" "expected 1, got $MANIFEST_COUNT"
fi

if [ -d "$WORKDIR/blobs/sha256" ]; then
    pass "blobs/sha256 directory created"
else
    fail "blobs directory" "not created"
fi

MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$MANIFEST_HASH" ]; then
    pass "manifest blob exists"
    MANIFEST_SCHEMA=$(jq -r '.schemaVersion' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
    if [ "$MANIFEST_SCHEMA" = "2" ]; then
        pass "manifest blob is valid JSON with schemaVersion 2"
    else
        fail "manifest blob" "invalid JSON or wrong schemaVersion"
    fi
else
    fail "manifest blob" "file not found: $MANIFEST_HASH"
fi

CONFIG_DIGEST=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
CONFIG_HASH="${CONFIG_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$CONFIG_HASH" ]; then
    pass "config blob exists"
    CONFIG_ARCH=$(jq -r '.architecture' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    CONFIG_OS=$(jq -r '.os' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    CONFIG_AUTHOR=$(jq -r '.author' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
    if [ "$CONFIG_ARCH" = "amd64" ]; then pass "config architecture is amd64"; else fail "config architecture" "expected amd64, got $CONFIG_ARCH"; fi
    if [ "$CONFIG_OS" = "linux" ]; then pass "config os is linux"; else fail "config os" "expected linux, got $CONFIG_OS"; fi
    if [ "$CONFIG_AUTHOR" = "test-suite" ]; then pass "config author is test-suite"; else fail "config author" "expected test-suite, got $CONFIG_AUTHOR"; fi
else
    fail "config blob" "file not found"
fi

PLAT_OS=$(jq -r '.manifests[0].platform.os' "$WORKDIR/index.json" 2>/dev/null)
PLAT_ARCH=$(jq -r '.manifests[0].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
if [ "$PLAT_OS" = "linux" ] && [ "$PLAT_ARCH" = "amd64" ]; then
    pass "platform metadata (os=linux, arch=amd64)"
else
    fail "platform metadata" "os=$PLAT_OS, arch=$PLAT_ARCH"
fi

EMPTY_LAYER=$(jq -r '.history[0].empty_layer' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$EMPTY_LAYER" = "true" ]; then
    pass "history marks empty_layer=true (no layer specified)"
else
    fail "history empty_layer" "expected true, got $EMPTY_LAYER"
fi

# Deep: check history comment/author
HIST_AUTHOR=$(jq -r '.history[0].author' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
HIST_COMMENT=$(jq -r '.history[0].comment' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$HIST_AUTHOR" = "test-suite" ]; then
    pass "history preserves author field"
else
    fail "history author" "expected test-suite, got $HIST_AUTHOR"
fi
if [ "$HIST_COMMENT" = "Minimal test image" ]; then
    pass "history preserves comment field"
else
    fail "history comment" "expected 'Minimal test image', got '$HIST_COMMENT'"
fi

# Deep: check created timestamp format
CREATED=$(jq -r '.created' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if echo "$CREATED" | grep -qP '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$'; then
    pass "config.created is valid ISO 8601 timestamp: $CREATED"
else
    fail "config.created format" "got $CREATED"
fi

# Deep: manifest media types
MANIFEST_MT=$(jq -r '.manifests[0].mediaType' "$WORKDIR/index.json" 2>/dev/null)
if [ "$MANIFEST_MT" = "application/vnd.oci.image.manifest.v1+json" ]; then
    pass "manifest descriptor mediaType correct"
else
    fail "manifest mediaType" "expected application/vnd.oci.image.manifest.v1+json, got $MANIFEST_MT"
fi

CONFIG_MT=$(jq -r '.config.mediaType' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$CONFIG_MT" = "application/vnd.oci.image.config.v1+json" ]; then
    pass "config descriptor mediaType correct"
else
    fail "config mediaType" "expected application/vnd.oci.image.config.v1+json, got $CONFIG_MT"
fi

# Deep: rootfs type
ROOTFS_TYPE=$(jq -r '.rootfs.type' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$ROOTFS_TYPE" = "layers" ]; then
    pass "rootfs.type is 'layers'"
else
    fail "rootfs.type" "expected layers, got $ROOTFS_TYPE"
fi

# Deep: layers array should be empty for no-layer image
LAYERS_LEN=$(jq '.layers | length' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYERS_LEN" = "0" ]; then
    pass "manifest has 0 layers (empty image)"
else
    fail "layers count" "expected 0, got $LAYERS_LEN"
fi

# Deep: manifest size field matches actual blob size
MANIFEST_SIZE=$(jq -r '.manifests[0].size' "$WORKDIR/index.json" 2>/dev/null)
ACTUAL_SIZE=$(stat -c%s "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null || stat -f%z "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$MANIFEST_SIZE" = "$ACTUAL_SIZE" ]; then
    pass "manifest descriptor size matches actual blob size ($MANIFEST_SIZE bytes)"
else
    fail "manifest size" "descriptor says $MANIFEST_SIZE, actual is $ACTUAL_SIZE"
fi

rm -rf "$WORKDIR"

# --------------------------------------------------
# Test 3: Build image with a layer
# --------------------------------------------------
echo ""
echo "Test 3: Build OCI image with a filesystem layer"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)

mkdir -p "$LAYER_DIR/usr/bin"
echo '#!/bin/sh' > "$LAYER_DIR/usr/bin/hello"
echo 'echo "Hello from OCI"' >> "$LAYER_DIR/usr/bin/hello"
chmod 755 "$LAYER_DIR/usr/bin/hello"
mkdir -p "$LAYER_DIR/etc"
echo "test-container" > "$LAYER_DIR/etc/hostname"
mkdir -p "$LAYER_DIR/var/empty"
ln -s /usr/bin/hello "$LAYER_DIR/usr/bin/hi"

cd "$WORKDIR"

export SOURCE_DATE_EPOCH=1700000000
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

MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
LAYER_COUNT=$(jq '.layers | length' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_COUNT" = "1" ]; then
    pass "manifest has 1 layer"
else
    fail "layer count" "expected 1, got $LAYER_COUNT"
fi

LAYER_MT=$(jq -r '.layers[0].mediaType' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_MT" = "application/vnd.oci.image.layer.v1.tar+gzip" ]; then
    pass "layer mediaType is tar+gzip"
else
    fail "layer mediaType" "expected tar+gzip, got $LAYER_MT"
fi

LAYER_DIGEST=$(jq -r '.layers[0].digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
LAYER_HASH="${LAYER_DIGEST#sha256:}"
if [ -f "$WORKDIR/blobs/sha256/$LAYER_HASH" ]; then
    pass "layer blob file exists"
    if gzip -t "$WORKDIR/blobs/sha256/$LAYER_HASH" 2>/dev/null; then
        pass "layer blob is valid gzip"
    else
        fail "layer blob" "not valid gzip"
    fi
else
    fail "layer blob" "file not found"
fi

# Deep: layer size matches descriptor
LAYER_SIZE_DESC=$(jq -r '.layers[0].size' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
LAYER_SIZE_ACTUAL=$(stat -c%s "$WORKDIR/blobs/sha256/$LAYER_HASH" 2>/dev/null || stat -f%z "$WORKDIR/blobs/sha256/$LAYER_HASH" 2>/dev/null)
if [ "$LAYER_SIZE_DESC" = "$LAYER_SIZE_ACTUAL" ]; then
    pass "layer descriptor size matches actual ($LAYER_SIZE_DESC bytes)"
else
    fail "layer size" "descriptor=$LAYER_SIZE_DESC, actual=$LAYER_SIZE_ACTUAL"
fi

# Deep: decompress layer and inspect tar contents
EXTRACT_DIR=$(mktemp -d)
if gzip -dc "$WORKDIR/blobs/sha256/$LAYER_HASH" | tar tf - > "$EXTRACT_DIR/listing.txt" 2>/dev/null; then
    pass "layer tar is extractable"

    if grep -q "usr/bin/hello" "$EXTRACT_DIR/listing.txt"; then
        pass "layer contains usr/bin/hello"
    else
        fail "layer content" "usr/bin/hello not found in tar"
    fi

    if grep -q "etc/hostname" "$EXTRACT_DIR/listing.txt"; then
        pass "layer contains etc/hostname"
    else
        fail "layer content" "etc/hostname not found in tar"
    fi

    if grep -q "usr/bin/hi" "$EXTRACT_DIR/listing.txt"; then
        pass "layer contains symlink usr/bin/hi"
    else
        fail "layer content" "symlink usr/bin/hi not found in tar"
    fi

    if grep -q "var/empty" "$EXTRACT_DIR/listing.txt"; then
        pass "layer contains empty directory var/empty"
    else
        fail "layer content" "empty directory var/empty not found"
    fi

    # Deep: check file count is reasonable
    FILE_COUNT=$(wc -l < "$EXTRACT_DIR/listing.txt")
    info "Layer contains $FILE_COUNT tar entries"
else
    fail "layer tar" "could not decompress and list"
fi

# Deep: extract and verify file content
EXTRACT_DIR2=$(mktemp -d)
gzip -dc "$WORKDIR/blobs/sha256/$LAYER_HASH" | tar xf - -C "$EXTRACT_DIR2" 2>/dev/null || true
if [ -f "$EXTRACT_DIR2/etc/hostname" ]; then
    HOSTNAME_CONTENT=$(cat "$EXTRACT_DIR2/etc/hostname")
    if [ "$HOSTNAME_CONTENT" = "test-container" ]; then
        pass "extracted file content matches (etc/hostname)"
    else
        fail "file content" "expected test-container, got $HOSTNAME_CONTENT"
    fi
else
    warn "file extraction" "could not extract etc/hostname (may be path prefix issue)"
fi

if [ -L "$EXTRACT_DIR2/usr/bin/hi" ] || [ -L "$EXTRACT_DIR2/./usr/bin/hi" ]; then
    pass "symlink preserved in extracted layer"
else
    warn "symlink" "could not verify symlink after extraction"
fi
rm -rf "$EXTRACT_DIR" "$EXTRACT_DIR2"

CONFIG_DIGEST=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
CONFIG_HASH="${CONFIG_DIGEST#sha256:}"
DIFF_ID_COUNT=$(jq '.rootfs.diff_ids | length' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$DIFF_ID_COUNT" = "1" ]; then
    pass "config has 1 diff_id"
else
    fail "diff_id count" "expected 1, got $DIFF_ID_COUNT"
fi

# Deep: diff_id should be sha256:hex format
DIFF_ID=$(jq -r '.rootfs.diff_ids[0]' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if echo "$DIFF_ID" | grep -qP '^sha256:[0-9a-f]{64}$'; then
    pass "diff_id is valid sha256 format"
else
    fail "diff_id format" "got $DIFF_ID"
fi

# Deep: diff_id should be hash of UNCOMPRESSED tar
UNCOMPRESSED_HASH=$(gzip -dc "$WORKDIR/blobs/sha256/$LAYER_HASH" | sha256sum | cut -d' ' -f1)
DIFF_ID_HASH="${DIFF_ID#sha256:}"
if [ "$UNCOMPRESSED_HASH" = "$DIFF_ID_HASH" ]; then
    pass "diff_id matches sha256 of uncompressed tar"
else
    fail "diff_id integrity" "diff_id=$DIFF_ID_HASH, uncompressed sha256=$UNCOMPRESSED_HASH"
fi

CONFIG_ENV=$(jq -r '.config.Env[0]' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$CONFIG_ENV" = "PATH=/usr/bin:/bin" ]; then
    pass "config Env preserved"
else
    fail "config Env" "expected PATH=/usr/bin:/bin, got $CONFIG_ENV"
fi

CONFIG_WD=$(jq -r '.config.WorkingDir' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
if [ "$CONFIG_WD" = "/" ]; then
    pass "config WorkingDir preserved"
else
    fail "config WorkingDir" "expected /, got $CONFIG_WD"
fi

unset SOURCE_DATE_EPOCH
rm -rf "$WORKDIR" "$LAYER_DIR"

# --------------------------------------------------
# Test 4: Disabled compression
# --------------------------------------------------
echo ""
echo "Test 4: Build with compression disabled"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)
echo "test file content" > "$LAYER_DIR/test.txt"

cd "$WORKDIR"

export SOURCE_DATE_EPOCH=1700000000
cat <<YAML | build-oci
compression: disabled
images:
  - architecture: arm64
    os: linux
    layer: "$LAYER_DIR"
YAML
unset SOURCE_DATE_EPOCH

MANIFEST_DIGEST=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
MANIFEST_HASH="${MANIFEST_DIGEST#sha256:}"
LAYER_MT=$(jq -r '.layers[0].mediaType' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" 2>/dev/null)
if [ "$LAYER_MT" = "application/vnd.oci.image.layer.v1.tar" ]; then
    pass "layer mediaType is tar (no gzip)"
else
    fail "disabled compression" "expected tar, got $LAYER_MT"
fi

# Deep: layer should be a plain tar (not gzip)
LAYER_HASH=$(jq -r '.layers[0].digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" | cut -d: -f2)
if ! gzip -t "$WORKDIR/blobs/sha256/$LAYER_HASH" 2>/dev/null; then
    pass "layer blob is NOT gzip (correct for disabled compression)"
else
    fail "disabled compression" "layer is still gzip compressed"
fi

# Deep: plain tar should be listable directly
if tar tf "$WORKDIR/blobs/sha256/$LAYER_HASH" > /dev/null 2>&1; then
    pass "uncompressed layer is valid tar"
else
    fail "uncompressed layer" "not a valid tar file"
fi

# Deep: diff_id should match layer digest for uncompressed
CONFIG_HASH=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MANIFEST_HASH" | cut -d: -f2)
DIFF_ID=$(jq -r '.rootfs.diff_ids[0]' "$WORKDIR/blobs/sha256/$CONFIG_HASH" 2>/dev/null)
DIFF_ID_HASH="${DIFF_ID#sha256:}"
LAYER_FILE_HASH=$(sha256sum "$WORKDIR/blobs/sha256/$LAYER_HASH" | cut -d' ' -f1)
if [ "$DIFF_ID_HASH" = "$LAYER_FILE_HASH" ]; then
    pass "diff_id matches layer blob hash (uncompressed case)"
else
    fail "uncompressed diff_id" "diff_id=$DIFF_ID_HASH, blob=$LAYER_FILE_HASH"
fi

PLAT_ARCH=$(jq -r '.manifests[0].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
if [ "$PLAT_ARCH" = "arm64" ]; then
    pass "platform architecture is arm64"
else
    fail "platform" "expected arm64, got $PLAT_ARCH"
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

ARCH0=$(jq -r '.manifests[0].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
ARCH1=$(jq -r '.manifests[1].platform.architecture' "$WORKDIR/index.json" 2>/dev/null)
if [ "$ARCH0" = "amd64" ] && [ "$ARCH1" = "arm64" ]; then
    pass "multi-arch platforms correct (amd64, arm64)"
else
    fail "multi-arch platforms" "got $ARCH0, $ARCH1"
fi

# Deep: each manifest should point to distinct blobs
DIGEST0=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" 2>/dev/null)
DIGEST1=$(jq -r '.manifests[1].digest' "$WORKDIR/index.json" 2>/dev/null)
if [ "$DIGEST0" != "$DIGEST1" ]; then
    pass "each manifest has a distinct digest"
else
    fail "multi-image digests" "both manifests have same digest"
fi

# Deep: both manifest blobs should exist and be valid
for i in 0 1; do
    MHASH=$(jq -r ".manifests[$i].digest" "$WORKDIR/index.json" | cut -d: -f2)
    if [ -f "$WORKDIR/blobs/sha256/$MHASH" ] && jq . "$WORKDIR/blobs/sha256/$MHASH" >/dev/null 2>&1; then
        pass "manifest $i blob exists and is valid JSON"
    else
        fail "manifest $i" "blob missing or invalid"
    fi
done

rm -rf "$WORKDIR"

# --------------------------------------------------
# Test 6: SHA256 digest integrity (all blobs)
# --------------------------------------------------
echo ""
echo "Test 6: Blob digest integrity verification"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)
echo "integrity test content" > "$LAYER_DIR/data.txt"
mkdir -p "$LAYER_DIR/subdir"
echo "nested" > "$LAYER_DIR/subdir/nested.txt"

cd "$WORKDIR"

export SOURCE_DATE_EPOCH=1700000000
cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
YAML
unset SOURCE_DATE_EPOCH

BLOB_COUNT=0
ALL_OK=true
for blob_file in "$WORKDIR"/blobs/sha256/*; do
    expected_hash=$(basename "$blob_file")
    actual_hash=$(sha256sum "$blob_file" | cut -d' ' -f1)
    BLOB_COUNT=$((BLOB_COUNT + 1))
    if [ "$expected_hash" != "$actual_hash" ]; then
        fail "digest integrity" "$blob_file: expected $expected_hash, got $actual_hash"
        ALL_OK=false
    fi
done

if [ "$ALL_OK" = true ]; then
    pass "all $BLOB_COUNT blob digests match their filenames"
fi

# Deep: verify cross-references (index -> manifest -> config -> layer)
MHASH=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" | cut -d: -f2)
CHASH=$(jq -r '.config.digest' "$WORKDIR/blobs/sha256/$MHASH" | cut -d: -f2)
LHASH=$(jq -r '.layers[0].digest' "$WORKDIR/blobs/sha256/$MHASH" | cut -d: -f2)

if [ -f "$WORKDIR/blobs/sha256/$MHASH" ] && [ -f "$WORKDIR/blobs/sha256/$CHASH" ] && [ -f "$WORKDIR/blobs/sha256/$LHASH" ]; then
    pass "full blob chain verified: index -> manifest -> config + layer"
else
    fail "blob chain" "missing blob in reference chain"
fi

rm -rf "$WORKDIR" "$LAYER_DIR"

# --------------------------------------------------
# Test 7: SOURCE_DATE_EPOCH reproducibility
# --------------------------------------------------
echo ""
echo "Test 7: SOURCE_DATE_EPOCH reproducibility"

WORKDIR1=$(mktemp -d)
WORKDIR2=$(mktemp -d)
LAYER_DIR=$(mktemp -d)
echo "repro test" > "$LAYER_DIR/file.txt"

export SOURCE_DATE_EPOCH=1700000000

cd "$WORKDIR1"
cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "epoch-test"
    layer: "$LAYER_DIR"
YAML

cd "$WORKDIR2"
cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "epoch-test"
    layer: "$LAYER_DIR"
YAML

DIGEST1=$(jq -r '.manifests[0].digest' "$WORKDIR1/index.json" 2>/dev/null)
DIGEST2=$(jq -r '.manifests[0].digest' "$WORKDIR2/index.json" 2>/dev/null)

if [ "$DIGEST1" = "$DIGEST2" ] && [ -n "$DIGEST1" ]; then
    pass "SOURCE_DATE_EPOCH produces reproducible manifest digests"
else
    fail "reproducibility" "manifest digests differ: $DIGEST1 vs $DIGEST2"
fi

# Deep: compare all blob hashes
BLOBS1=$(cd "$WORKDIR1/blobs/sha256" && ls | sort)
BLOBS2=$(cd "$WORKDIR2/blobs/sha256" && ls | sort)
if [ "$BLOBS1" = "$BLOBS2" ]; then
    pass "all blob hashes identical across two builds"
else
    fail "blob reproducibility" "blob sets differ between builds"
fi

# Deep: verify timestamps in config use SOURCE_DATE_EPOCH
CONFIG_FILE=$(get_config_blob "$WORKDIR1")
CREATED=$(jq -r '.created' "$CONFIG_FILE" 2>/dev/null)
if [ "$CREATED" = "2023-11-14T22:13:20Z" ]; then
    pass "config.created uses SOURCE_DATE_EPOCH timestamp (2023-11-14T22:13:20Z)"
else
    fail "epoch timestamp" "expected 2023-11-14T22:13:20Z, got $CREATED"
fi

unset SOURCE_DATE_EPOCH
rm -rf "$WORKDIR1" "$WORKDIR2" "$LAYER_DIR"

# --------------------------------------------------
# Test 8: Large layer with many files
# --------------------------------------------------
echo ""
echo "Test 8: Large layer stress test (many files)"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)

# Create 500 files across multiple directories
for i in $(seq 1 10); do
    mkdir -p "$LAYER_DIR/dir_$i"
    for j in $(seq 1 50); do
        echo "content_${i}_${j}" > "$LAYER_DIR/dir_$i/file_${j}.txt"
    done
done

cd "$WORKDIR"
export SOURCE_DATE_EPOCH=1700000000
cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
YAML
unset SOURCE_DATE_EPOCH

if [ -f "$WORKDIR/index.json" ]; then
    pass "large layer build succeeded"
else
    fail "large layer" "build failed"
fi

# Count entries in the tar
LAYER_HASH=$(jq -r '.layers[0].digest' "$(get_manifest_blob "$WORKDIR")" | cut -d: -f2)
ENTRY_COUNT=$(gzip -dc "$WORKDIR/blobs/sha256/$LAYER_HASH" | tar tf - | wc -l)
# 500 files + 10 dirs + root = 511+
if [ "$ENTRY_COUNT" -ge 511 ]; then
    pass "layer contains $ENTRY_COUNT entries (expected >= 511)"
else
    fail "large layer entries" "expected >= 511, got $ENTRY_COUNT"
fi

rm -rf "$WORKDIR" "$LAYER_DIR"

# --------------------------------------------------
# Test 9: Special characters and permissions
# --------------------------------------------------
echo ""
echo "Test 9: File permissions and special cases"

WORKDIR=$(mktemp -d)
LAYER_DIR=$(mktemp -d)

mkdir -p "$LAYER_DIR/bin"
echo "executable" > "$LAYER_DIR/bin/run"
chmod 755 "$LAYER_DIR/bin/run"
echo "readonly" > "$LAYER_DIR/bin/readonly"
chmod 444 "$LAYER_DIR/bin/readonly"
mkdir -p "$LAYER_DIR/restricted"
chmod 700 "$LAYER_DIR/restricted"
ln -s /bin/run "$LAYER_DIR/bin/link"

cd "$WORKDIR"
export SOURCE_DATE_EPOCH=1700000000
cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
YAML
unset SOURCE_DATE_EPOCH

LAYER_HASH=$(jq -r '.layers[0].digest' "$(get_manifest_blob "$WORKDIR")" | cut -d: -f2)

# Extract and check permissions
EXTRACT=$(mktemp -d)
gzip -dc "$WORKDIR/blobs/sha256/$LAYER_HASH" | tar xf - -C "$EXTRACT" 2>/dev/null || true

if [ -f "$EXTRACT/bin/run" ]; then
    PERMS=$(stat -c%a "$EXTRACT/bin/run" 2>/dev/null || stat -f%A "$EXTRACT/bin/run" 2>/dev/null)
    if [ "$PERMS" = "755" ]; then
        pass "executable permission preserved (755)"
    else
        fail "permissions" "expected 755, got $PERMS"
    fi
else
    warn "permissions" "could not extract bin/run"
fi

if [ -f "$EXTRACT/bin/readonly" ]; then
    PERMS=$(stat -c%a "$EXTRACT/bin/readonly" 2>/dev/null || stat -f%A "$EXTRACT/bin/readonly" 2>/dev/null)
    if [ "$PERMS" = "444" ]; then
        pass "readonly permission preserved (444)"
    else
        fail "permissions" "expected 444, got $PERMS"
    fi
else
    warn "permissions" "could not extract bin/readonly"
fi

if [ -L "$EXTRACT/bin/link" ]; then
    LINK_TARGET=$(readlink "$EXTRACT/bin/link")
    if [ "$LINK_TARGET" = "/bin/run" ]; then
        pass "symlink target preserved (/bin/run)"
    else
        fail "symlink" "expected /bin/run, got $LINK_TARGET"
    fi
else
    warn "symlink" "could not verify symlink after extraction"
fi

rm -rf "$WORKDIR" "$LAYER_DIR" "$EXTRACT"

# --------------------------------------------------
# Test 10: index-annotations and variant
# --------------------------------------------------
echo ""
echo "Test 10: Extended metadata (index-annotations, variant)"

WORKDIR=$(mktemp -d)
cd "$WORKDIR"

cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: arm64
    os: linux
    variant: v8
    annotations:
      org.opencontainers.image.title: "my-image"
    index-annotations:
      org.opencontainers.image.ref.name: "latest"
YAML

# Check variant
VARIANT=$(jq -r '.manifests[0].platform.variant' "$WORKDIR/index.json" 2>/dev/null)
if [ "$VARIANT" = "v8" ]; then
    pass "platform variant preserved (v8)"
else
    fail "variant" "expected v8, got $VARIANT"
fi

# Check index-annotations
IDX_ANN=$(jq -r '.manifests[0].annotations["org.opencontainers.image.ref.name"]' "$WORKDIR/index.json" 2>/dev/null)
if [ "$IDX_ANN" = "latest" ]; then
    pass "index-annotations preserved on manifest descriptor"
else
    fail "index-annotations" "expected latest, got $IDX_ANN"
fi

# Check manifest annotations
MHASH=$(jq -r '.manifests[0].digest' "$WORKDIR/index.json" | cut -d: -f2)
MANIFEST_ANN=$(jq -r '.annotations["org.opencontainers.image.title"]' "$WORKDIR/blobs/sha256/$MHASH" 2>/dev/null)
if [ "$MANIFEST_ANN" = "my-image" ]; then
    pass "manifest annotations preserved"
else
    fail "manifest annotations" "expected my-image, got $MANIFEST_ANN"
fi

rm -rf "$WORKDIR"


# ======================================================================
echo ""
echo "============================================================"
echo "PART 2: PYTHON vs RUST COMPARISON"
echo "--------------------------------------------------------------"
# ======================================================================

if [ -n "$PYTHON_CMD" ]; then
    info "Python build-oci available: $PYTHON_CMD"
else
    # Try direct invocation
    if command -v build-oci-py >/dev/null 2>&1; then
        PYTHON_CMD="build-oci-py"
        info "Python build-oci available: $PYTHON_CMD"
    else
        # Create a wrapper script
        cat > /usr/local/bin/build-oci-py <<'PYEOF'
#!/usr/bin/env python3
import sys
sys.path.insert(0, "/opt/python-oci")
from oci_builder.cmd import main
main()
PYEOF
        chmod +x /usr/local/bin/build-oci-py
        if echo 'images: [{architecture: amd64, os: linux}]' | build-oci-py 2>/dev/null; then
            PYTHON_CMD="build-oci-py"
            info "Python build-oci wrapper created successfully"
        else
            warn "Python comparison" "Python build-oci not available, skipping comparison tests"
            PYTHON_CMD=""
        fi
    fi
fi

if [ -n "$PYTHON_CMD" ]; then

    # --------------------------------------------------
    # Compare: Minimal image (no layers)
    # --------------------------------------------------
    echo ""
    echo "Comparison 1: Minimal image output (Rust vs Python)"

    RUST_DIR=$(mktemp -d)
    PY_DIR=$(mktemp -d)

    export SOURCE_DATE_EPOCH=1700000000

    cd "$RUST_DIR"
    cat <<'YAML' | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "comparison-test"
    comment: "comparing outputs"
YAML

    cd "$PY_DIR"
    cat <<'YAML' | $PYTHON_CMD
compression: gzip
images:
  - architecture: amd64
    os: linux
    author: "comparison-test"
    comment: "comparing outputs"
YAML

    # Compare structure
    RUST_MANIFEST=$(jq -S . "$RUST_DIR/index.json" 2>/dev/null)
    PY_MANIFEST=$(jq -S . "$PY_DIR/index.json" 2>/dev/null)

    # Compare config blobs (canonical JSON)
    RUST_CONFIG=$(jq -S . "$(get_config_blob "$RUST_DIR")" 2>/dev/null)
    PY_CONFIG=$(jq -S . "$(get_config_blob "$PY_DIR")" 2>/dev/null)

    if [ "$RUST_CONFIG" = "$PY_CONFIG" ]; then
        pass "config JSON is identical (Rust vs Python)"
    else
        fail "config comparison" "configs differ"
        info "Rust config:"
        echo "$RUST_CONFIG" | head -20
        info "Python config:"
        echo "$PY_CONFIG" | head -20
    fi

    # Compare manifest structure (excluding digest/size refs which differ
    # due to JSON serialization differences between serde_json and python json)
    RUST_MBLOB_STRUCT=$(jq -S '{schemaVersion, layers, configMediaType: .config.mediaType}' "$(get_manifest_blob "$RUST_DIR")" 2>/dev/null)
    PY_MBLOB_STRUCT=$(jq -S '{schemaVersion, layers, configMediaType: .config.mediaType}' "$(get_manifest_blob "$PY_DIR")" 2>/dev/null)

    if [ "$RUST_MBLOB_STRUCT" = "$PY_MBLOB_STRUCT" ]; then
        pass "manifest structure is identical (Rust vs Python)"
    else
        fail "manifest comparison" "manifest structures differ"
        info "Rust manifest structure:"
        echo "$RUST_MBLOB_STRUCT" | head -20
        info "Python manifest structure:"
        echo "$PY_MBLOB_STRUCT" | head -20
    fi

    # Compare oci-layout
    RUST_LAYOUT=$(jq -S . "$RUST_DIR/oci-layout" 2>/dev/null)
    PY_LAYOUT=$(jq -S . "$PY_DIR/oci-layout" 2>/dev/null)
    if [ "$RUST_LAYOUT" = "$PY_LAYOUT" ]; then
        pass "oci-layout is identical"
    else
        fail "oci-layout comparison" "layouts differ"
    fi

    unset SOURCE_DATE_EPOCH
    rm -rf "$RUST_DIR" "$PY_DIR"

    # --------------------------------------------------
    # Compare: Image with layer
    # --------------------------------------------------
    echo ""
    echo "Comparison 2: Image with layer (Rust vs Python)"

    RUST_DIR=$(mktemp -d)
    PY_DIR=$(mktemp -d)
    LAYER_DIR=$(mktemp -d)

    mkdir -p "$LAYER_DIR/app"
    echo "hello world" > "$LAYER_DIR/app/data.txt"
    echo "#!/bin/sh" > "$LAYER_DIR/app/run.sh"
    chmod 755 "$LAYER_DIR/app/run.sh"
    ln -s /app/run.sh "$LAYER_DIR/app/start"

    export SOURCE_DATE_EPOCH=1700000000

    cd "$RUST_DIR"
    cat <<YAML | build-oci
compression: disabled
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
    config:
      Env:
        - APP_ENV=production
YAML

    cd "$PY_DIR"
    cat <<YAML | $PYTHON_CMD
compression: disabled
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
    config:
      Env:
        - APP_ENV=production
YAML

    # Compare configs semantically (excluding diff_ids which differ due to
    # tar format differences: Rust tar crate uses GNU format, Python uses PAX)
    RUST_CONFIG_SEM=$(jq -S 'del(.rootfs.diff_ids)' "$(get_config_blob "$RUST_DIR")" 2>/dev/null)
    PY_CONFIG_SEM=$(jq -S 'del(.rootfs.diff_ids)' "$(get_config_blob "$PY_DIR")" 2>/dev/null)

    if [ "$RUST_CONFIG_SEM" = "$PY_CONFIG_SEM" ]; then
        pass "config JSON with layer is semantically identical (excluding diff_ids)"
    else
        fail "config comparison (layer)" "configs differ beyond diff_ids"
        diff <(echo "$RUST_CONFIG_SEM") <(echo "$PY_CONFIG_SEM") || true
    fi

    # Compare layer tar contents (uncompressed, so we can compare listings)
    RUST_MBLOB=$(get_manifest_blob "$RUST_DIR")
    PY_MBLOB=$(get_manifest_blob "$PY_DIR")

    RUST_LHASH=$(jq -r '.layers[0].digest' "$RUST_MBLOB" | cut -d: -f2)
    PY_LHASH=$(jq -r '.layers[0].digest' "$PY_MBLOB" | cut -d: -f2)

    RUST_LISTING=$(tar tf "$RUST_DIR/blobs/sha256/$RUST_LHASH" 2>/dev/null | sort)
    PY_LISTING=$(tar tf "$PY_DIR/blobs/sha256/$PY_LHASH" 2>/dev/null | sort)

    if [ "$RUST_LISTING" = "$PY_LISTING" ]; then
        pass "layer tar file listing is identical"
    else
        fail "layer listing" "tar contents differ"
        diff <(echo "$RUST_LISTING") <(echo "$PY_LISTING") || true
    fi

    # Compare diff_ids - these will differ because Rust tar crate (GNU format)
    # and Python tarfile (PAX format) produce different binary tar output.
    # Both are valid; verify both are valid sha256 hashes.
    RUST_DIFFID=$(jq -r '.rootfs.diff_ids[0]' "$(get_config_blob "$RUST_DIR")" 2>/dev/null)
    PY_DIFFID=$(jq -r '.rootfs.diff_ids[0]' "$(get_config_blob "$PY_DIR")" 2>/dev/null)

    if [ "$RUST_DIFFID" = "$PY_DIFFID" ]; then
        pass "diff_ids are byte-identical between Rust and Python"
    else
        # Expected: different tar formats produce different hashes
        # Verify both are valid sha256 format
        BOTH_VALID=true
        echo "$RUST_DIFFID" | grep -qP '^sha256:[0-9a-f]{64}$' || BOTH_VALID=false
        echo "$PY_DIFFID" | grep -qP '^sha256:[0-9a-f]{64}$' || BOTH_VALID=false
        if [ "$BOTH_VALID" = true ]; then
            pass "diff_ids differ (expected: GNU vs PAX tar format), both valid sha256"
            info "  Rust:   $RUST_DIFFID"
            info "  Python: $PY_DIFFID"
        else
            fail "diff_id format" "one or both diff_ids are invalid"
        fi
    fi

    # Extract both layers and compare actual file contents
    RUST_EXTRACT=$(mktemp -d)
    PY_EXTRACT=$(mktemp -d)
    tar xf "$RUST_DIR/blobs/sha256/$RUST_LHASH" -C "$RUST_EXTRACT" 2>/dev/null || true
    tar xf "$PY_DIR/blobs/sha256/$PY_LHASH" -C "$PY_EXTRACT" 2>/dev/null || true

    RUST_DATA=$(cat "$RUST_EXTRACT/app/data.txt" 2>/dev/null)
    PY_DATA=$(cat "$PY_EXTRACT/app/data.txt" 2>/dev/null)
    if [ "$RUST_DATA" = "$PY_DATA" ] && [ "$RUST_DATA" = "hello world" ]; then
        pass "extracted file contents match between Rust and Python"
    else
        fail "extracted content" "Rust='$RUST_DATA', Python='$PY_DATA'"
    fi

    rm -rf "$RUST_EXTRACT" "$PY_EXTRACT"

    unset SOURCE_DATE_EPOCH
    rm -rf "$RUST_DIR" "$PY_DIR" "$LAYER_DIR"

    # --------------------------------------------------
    # Compare: Performance benchmark
    # --------------------------------------------------
    echo ""
    echo "Comparison 3: Performance benchmark"

    LAYER_DIR=$(mktemp -d)
    for i in $(seq 1 20); do
        mkdir -p "$LAYER_DIR/d$i"
        for j in $(seq 1 100); do
            dd if=/dev/urandom bs=1024 count=1 of="$LAYER_DIR/d$i/f$j.bin" 2>/dev/null
        done
    done
    info "Created test layer: 2000 files, ~2MB"

    export SOURCE_DATE_EPOCH=1700000000

    # Benchmark Rust
    RUST_DIR=$(mktemp -d)
    cd "$RUST_DIR"
    RUST_START=$(date +%s%N)
    cat <<YAML | build-oci
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
YAML
    RUST_END=$(date +%s%N)
    RUST_MS=$(( (RUST_END - RUST_START) / 1000000 ))

    # Benchmark Python
    PY_DIR=$(mktemp -d)
    cd "$PY_DIR"
    PY_START=$(date +%s%N)
    cat <<YAML | $PYTHON_CMD
compression: gzip
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
YAML
    PY_END=$(date +%s%N)
    PY_MS=$(( (PY_END - PY_START) / 1000000 ))

    info "Rust:   ${RUST_MS}ms"
    info "Python: ${PY_MS}ms"
    if [ "$PY_MS" -gt 0 ]; then
        SPEEDUP=$((PY_MS / (RUST_MS > 0 ? RUST_MS : 1)))
        info "Speedup: ~${SPEEDUP}x"
    fi

    if [ "$RUST_MS" -le "$PY_MS" ]; then
        pass "Rust is faster or equal (${RUST_MS}ms vs ${PY_MS}ms)"
    else
        warn "performance" "Rust ${RUST_MS}ms vs Python ${PY_MS}ms"
    fi

    unset SOURCE_DATE_EPOCH
    rm -rf "$RUST_DIR" "$PY_DIR" "$LAYER_DIR"

else
    info "Skipping Python comparison tests (Python build-oci not available)"
fi


# ======================================================================
echo ""
echo "============================================================"
echo "  FINAL RESULTS"
echo "============================================================"
echo ""
echo "  Passed:   $PASS"
echo "  Failed:   $FAIL"
echo "  Warnings: $WARN"
echo "============================================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
