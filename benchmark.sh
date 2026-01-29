#!/bin/bash
set -e

echo "============================================================"
echo "  build-oci - Benchmark Suite"
echo "============================================================"

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

WORKDIR=$(mktemp -d)
LAYER_DIR="$WORKDIR/layer"
OUTPUT_RUST="$WORKDIR/output_rust"
OUTPUT_PY="$WORKDIR/output_py"

mkdir -p "$LAYER_DIR"
mkdir -p "$OUTPUT_RUST"
mkdir -p "$OUTPUT_PY"

# Generate test data (adjust size as needed)
echo "[INFO] Generating test data in $LAYER_DIR..."
# 1000 small files with text content
for i in $(seq 1 1000); do
    echo "This is some compressible content for file $i repeated multiple times..." > "$LAYER_DIR/file_$i.txt"
done
# A few larger files (10MB) - COMPRESSIBLE
yes "pattern" | dd of="$LAYER_DIR/large_1.bin" bs=1M count=10 status=none
yes "anotherpattern" | dd of="$LAYER_DIR/large_2.bin" bs=1M count=10 status=none

echo "[INFO] Test data generated."

run_benchmark() {
    local name="$1"
    local cmd="$2"
    local output_dir="$3"
    local compression="$4"

    echo "------------------------------------------------------------"
    echo "Running $name (Compression: $compression)..."
    
    cd "$output_dir"
    
    # Create input YAML
    cat <<EOF > input.yaml
compression: $compression
images:
  - architecture: amd64
    os: linux
    layer: "$LAYER_DIR"
EOF

    # Run with time, output to stderr->stdout
    /usr/bin/time -v sh -c "$cmd < input.yaml" 2>&1
    
    local exit_code=$?
    if [ $exit_code -ne 0 ]; then
        echo "[FAIL] $name failed with exit code $exit_code"
        return 1
    fi

    # Check output size
    echo "  Output sizes:"
    du -h "$output_dir/blobs/sha256"/*

}

# Run benchmarks
run_benchmark "Rust" "build-oci" "$OUTPUT_RUST" "gzip"
run_benchmark "Python" "$PYTHON_CMD" "$OUTPUT_PY" "gzip"

echo "============================================================"
echo "Done."
rm -rf "$WORKDIR"
