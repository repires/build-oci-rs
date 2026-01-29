#!/bin/bash
set -e

echo "============================================================"
echo "  build-oci - Benchmark Suite"
echo "============================================================"

# Parse arguments
SCALE=${1:-20}  # Default: ~20MB test data
if [[ "$SCALE" =~ ^[0-9]+$ ]]; then
    echo "[INFO] Scale factor: ${SCALE}MB test data"
else
    echo "[ERROR] Invalid scale factor. Usage: $0 [scale_mb]"
    echo "  Examples:"
    echo "    $0        # ~20MB (default)"
    echo "    $0 100    # ~100MB"
    echo "    $0 1000   # ~1GB"
    exit 1
fi

# Ensure both binaries are available
if command -v build-oci >/dev/null; then
    echo "[INFO] Rust binary found: $(command -v build-oci)"
else
    echo "[ERROR] Rust binary (build-oci) not found!"
    exit 1
fi

PYTHON_CMD=""
if command -v build-oci-py >/dev/null; then
    PYTHON_CMD="build-oci-py"
    echo "[INFO] Python binary found: build-oci-py"
elif [ -f "/usr/local/bin/build-oci-py" ]; then
    PYTHON_CMD="/usr/local/bin/build-oci-py"
    echo "[INFO] Python binary found: /usr/local/bin/build-oci-py"
else
    # Fallback to direct python invocation if installed in /opt/python-oci
    PYTHON_CMD="python3 -c 'import sys; sys.path.insert(0,\"/opt/python-oci\"); from oci_builder.cmd import main; main()'"
    echo "[INFO] Using Python fallback command"
fi

# Detect time command (GNU time vs BSD time)
TIME_CMD=""
if command -v gtime >/dev/null 2>&1; then
    # macOS with GNU time installed via Homebrew (brew install gnu-time)
    TIME_CMD="gtime -v"
    echo "[INFO] Using GNU time (gtime) for measurements"
elif [ -x /usr/bin/time ] && /usr/bin/time --version 2>&1 | grep -q GNU; then
    # Linux with GNU time
    TIME_CMD="/usr/bin/time -v"
    echo "[INFO] Using GNU time for measurements"
elif [ -x /usr/bin/time ]; then
    # BSD time (macOS default) - limited output
    TIME_CMD="/usr/bin/time -l"
    echo "[INFO] Using BSD time for measurements (install gnu-time for detailed stats)"
else
    TIME_CMD="time"
    echo "[WARN] No suitable time command found, using shell builtin"
fi

WORKDIR=$(mktemp -d)
LAYER_DIR="$WORKDIR/layer"
OUTPUT_RUST="$WORKDIR/output_rust"
OUTPUT_PY="$WORKDIR/output_py"

mkdir -p "$LAYER_DIR"
mkdir -p "$OUTPUT_RUST"
mkdir -p "$OUTPUT_PY"

# Generate test data based on scale factor
echo "[INFO] Generating ~${SCALE}MB test data in $LAYER_DIR..."

if [ "$SCALE" -le 20 ]; then
    # Small test: ~20MB (1000 small files + 2x10MB large)
    for i in $(seq 1 1000); do
        echo "This is some compressible content for file $i repeated multiple times..." > "$LAYER_DIR/file_$i.txt"
    done
    yes "pattern" | dd of="$LAYER_DIR/large_1.bin" bs=1M count=10 status=none
    yes "anotherpattern" | dd of="$LAYER_DIR/large_2.bin" bs=1M count=10 status=none
elif [ "$SCALE" -le 100 ]; then
    # Medium test: ~100MB (10,000 x 10KB files)
    echo "[INFO] Creating 10,000 x 10KB files..."
    for i in $(seq 1 10000); do
        # Generate ~10KB of compressible content per file
        printf "%.0s$i content line " {1..200} > "$LAYER_DIR/file_$i.txt"
    done
elif [ "$SCALE" -le 500 ]; then
    # Large test: ~500MB (5,000 x 100KB files)
    echo "[INFO] Creating 5,000 x 100KB files..."
    for i in $(seq 1 5000); do
        # Generate ~100KB of compressible content per file
        printf "%.0s$i content line repeated many times to make this file larger " {1..1500} > "$LAYER_DIR/file_$i.txt"
    done
else
    # Very large test: ~1GB+ (1,000 x 1MB files + 10,000 x 10KB files)
    echo "[INFO] Creating 1,000 x 1MB files + 10,000 x 10KB files..."
    mkdir -p "$LAYER_DIR/large"
    mkdir -p "$LAYER_DIR/small"

    # Create 1,000 x 1MB files using dd (faster than printf)
    for i in $(seq 1 1000); do
        yes "pattern$i" | dd of="$LAYER_DIR/large/file_$i.bin" bs=1M count=1 status=none
    done

    # Create 10,000 x 10KB files
    for i in $(seq 1 10000); do
        printf "%.0s$i content line " {1..200} > "$LAYER_DIR/small/file_$i.txt"
    done
fi

echo "[INFO] Test data generated. Actual size:"
du -sh "$LAYER_DIR"

run_benchmark() {
    local name="$1"
    local cmd="$2"
    local output_dir="$3"
    local compression="$4"

    echo "------------------------------------------------------------"
    echo "Running $name (Compression: $compression)..."

    # Clean output directory
    rm -rf "$output_dir"/*

    cd "$output_dir"

    # Create input YAML
    cat <<EOF > input.yaml
compression: $compression
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
EOF

    # Run with time
    if [ -n "$TIME_CMD" ]; then
        $TIME_CMD sh -c "$cmd < input.yaml" 2>&1
    else
        sh -c "$cmd < input.yaml"
    fi

    local exit_code=$?
    if [ $exit_code -ne 0 ]; then
        echo "[FAIL] $name failed with exit code $exit_code"
        return 1
    fi

    # Check output size
    echo "  Output sizes:"
    du -h "$output_dir/blobs/sha256"/* 2>/dev/null || echo "  (no blobs)"
    echo ""
}

# Run benchmarks with different compression settings
echo ""
echo "============================================================"
echo "  GZIP Compression Benchmarks"
echo "============================================================"
run_benchmark "Rust (gzip)" "build-oci" "$OUTPUT_RUST" "gzip"
run_benchmark "Python (gzip)" "$PYTHON_CMD" "$OUTPUT_PY" "gzip"

echo ""
echo "============================================================"
echo "  ZSTD Compression Benchmarks"
echo "============================================================"
run_benchmark "Rust (zstd)" "build-oci" "$OUTPUT_RUST" "zstd"

echo ""
echo "============================================================"
echo "  No Compression Benchmarks"
echo "============================================================"
run_benchmark "Rust (disabled)" "build-oci" "$OUTPUT_RUST" "disabled"

echo "============================================================"
echo "Done. Cleaning up..."
rm -rf "$WORKDIR"
echo "Benchmark complete."
