#!/usr/bin/env bash
# Run all RVO load-harness scenarios and produce CSVs in target/bench_results/.
#
# REQUIREMENTS:
#   - Run on bare-metal Linux (or a dedicated VM) — NOT WSL. WSL introduces
#     hypervisor jitter that makes p99/p99.9 meaningless.
#   - CPU governor set to performance:
#       sudo cpupower frequency-set -g performance
#   - Turbo disabled (optional but recommended):
#       echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
#   - Release build (script enforces this via RUSTFLAGS and --release).
#
# Usage:
#   bash scripts/bench.sh               # all scenarios, 30s each
#   bash scripts/bench.sh --duration 60 # 60s per scenario
#   bash scripts/bench.sh --scenarios "baseline blocking_3ms fps_30"
set -euo pipefail

DURATION=${DURATION:-30}
WARMUP=${WARMUP:-5}
OUT_DIR="target/bench_results"
HARNESS="target/release/load_harness"

ALL_SCENARIOS=(
    baseline
    inproc_low
    blocking_1ms
    blocking_3ms
    blocking_10ms
    blocking_50ms
    load_shed
    fps_30
    fps_60
    fps_120
    fps_300
)

# Parse optional overrides.
while [[ $# -gt 0 ]]; do
    case "$1" in
        --duration) DURATION="$2"; shift 2 ;;
        --warmup)   WARMUP="$2";   shift 2 ;;
        --scenarios) IFS=' ' read -r -a ALL_SCENARIOS <<< "$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# Build in release (enforces LTO=true, codegen-units=1 from Cargo.toml profiles).
echo "[bench] Building load_harness (release) ..."
cargo build -p rvo-bench --bin load_harness --release 2>&1

mkdir -p "$OUT_DIR"
# Remove stale summary so we don't append to an old run.
rm -f "$OUT_DIR/summary.csv"

echo "[bench] Starting scenarios (duration=${DURATION}s, warmup=${WARMUP}s)"
for SCENARIO in "${ALL_SCENARIOS[@]}"; do
    echo ""
    echo "══════════════════════════════════════════════"
    echo " Scenario: $SCENARIO"
    echo "══════════════════════════════════════════════"
    "$HARNESS" \
        --scenario "$SCENARIO" \
        --duration-secs "$DURATION" \
        --warmup-secs "$WARMUP" \
        --out-dir "$OUT_DIR"
    # Brief gap between runs to let the OS settle.
    sleep 2
done

echo ""
echo "[bench] All done. Results in $OUT_DIR/"
echo "  summary:      $OUT_DIR/summary.csv"
echo "  time-series:  $OUT_DIR/*_timeseries.csv"
echo ""
echo "To generate figures:"
echo "  python3 scripts/plot.py --in-dir $OUT_DIR --out-dir docs/report/figures"
