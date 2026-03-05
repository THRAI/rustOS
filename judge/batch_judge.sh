#!/usr/bin/env bash
# QEMU + testsuits 批量判分脚本（基于日志）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUSTOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNNER="$SCRIPT_DIR/basic/test_runner.py"

TARGET="oscomp-basic"
TIMEOUT_SECS=1200
INPUT_LOG=""
KEEP_LOG=0

usage() {
    cat <<'EOF'
Usage:
  ./judge/batch_judge.sh [--target oscomp-basic|oscomp-basic-all|oscomp] [--timeout SEC] [--keep-log]
  ./judge/batch_judge.sh --log <log_file>

Examples:
  ./judge/batch_judge.sh
  ./judge/batch_judge.sh --target oscomp-basic-all --keep-log
  ./judge/batch_judge.sh --log /tmp/oscomp.log
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            TARGET="${2:-}"
            shift 2
            ;;
        --timeout)
            TIMEOUT_SECS="${2:-}"
            shift 2
            ;;
        --log)
            INPUT_LOG="${2:-}"
            shift 2
            ;;
        --keep-log)
            KEEP_LOG=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

if [[ ! -f "$RUNNER" ]]; then
    echo "Runner not found: $RUNNER" >&2
    exit 2
fi

LOG_FILE=""
if [[ -n "$INPUT_LOG" ]]; then
    if [[ ! -f "$INPUT_LOG" ]]; then
        echo "Log file not found: $INPUT_LOG" >&2
        exit 2
    fi
    LOG_FILE="$INPUT_LOG"
else
    LOG_FILE="$(mktemp)"
    # oscomp* targets in this repo require sudo (mount/cp/umount image)
    # Pre-auth once here to avoid hanging on password prompt in piped make output.
    if [[ "$TARGET" == oscomp* ]]; then
        echo "=== sudo pre-auth required for image mount/copy ==="
        sudo -v
    fi
    echo "=== Running: make $TARGET (timeout=${TIMEOUT_SECS}s) ==="
    set +e
    if command -v script >/dev/null 2>&1; then
        (
            cd "$RUSTOS_DIR"
            # Use a PTY so QEMU output is shown immediately and sudo prompts are visible.
            script -qefc "timeout $TIMEOUT_SECS make $TARGET" "$LOG_FILE"
        )
        MAKE_STATUS=$?
    else
        (
            cd "$RUSTOS_DIR"
            timeout "$TIMEOUT_SECS" make "$TARGET"
        ) 2>&1 | tee "$LOG_FILE"
        MAKE_STATUS=${PIPESTATUS[0]}
    fi
    set -e
    if [[ $MAKE_STATUS -ne 0 ]]; then
        echo "=== make $TARGET exited with status $MAKE_STATUS ===" >&2
    fi
fi

extract_group() {
    local group="$1"
    local out="$2"
    sed -n "/#### OS COMP TEST GROUP START ${group} ####/,/#### OS COMP TEST GROUP END ${group} ####/p" "$LOG_FILE" > "$out"
}

judge_one_log() {
    local name="$1"
    local log="$2"
    local json_file="$3"
    PYTHONWARNINGS=ignore python3 "$RUNNER" "$log" > "$json_file"
    python3 - "$name" "$json_file" <<'PY'
import json, sys
name, path = sys.argv[1], sys.argv[2]
data = json.load(open(path))
total = sum(x["all"] for x in data)
passed = sum(x["passed"] for x in data)
fails = [x for x in data if x["passed"] < x["all"]]
print(f"[{name}] checks: {passed}/{total}")
if fails:
    print(f"[{name}] failed cases:")
    for x in fails:
        print(f"  - {x['name']}: {x['passed']}/{x['all']}")
    sys.exit(1)
print(f"[{name}] all passed")
PY
}

tmp_musl="$(mktemp)"
tmp_glibc="$(mktemp)"
tmp_json_musl="$(mktemp)"
tmp_json_glibc="$(mktemp)"

extract_group "basic-musl" "$tmp_musl"
extract_group "basic-glibc" "$tmp_glibc"

HAVE_MUSL=0
HAVE_GLIBC=0
[[ -s "$tmp_musl" ]] && HAVE_MUSL=1
[[ -s "$tmp_glibc" ]] && HAVE_GLIBC=1

STATUS=0
if [[ $HAVE_MUSL -eq 1 ]]; then
    judge_one_log "basic-musl" "$tmp_musl" "$tmp_json_musl" || STATUS=1
fi
if [[ $HAVE_GLIBC -eq 1 ]]; then
    judge_one_log "basic-glibc" "$tmp_glibc" "$tmp_json_glibc" || STATUS=1
fi

if [[ $HAVE_MUSL -eq 0 && $HAVE_GLIBC -eq 0 ]]; then
    echo "No basic group markers found, fallback to whole log."
    judge_one_log "raw-log" "$LOG_FILE" "$tmp_json_musl" || STATUS=1
fi

if [[ -z "$INPUT_LOG" && $KEEP_LOG -eq 0 ]]; then
    rm -f "$LOG_FILE"
fi
rm -f "$tmp_musl" "$tmp_glibc" "$tmp_json_musl" "$tmp_json_glibc"

exit $STATUS
