#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_FILE="$(mktemp)"
QEMU_PID=""

cleanup() {
    if [[ -n "${QEMU_PID}" ]] && kill -0 "${QEMU_PID}" 2>/dev/null; then
        kill "${QEMU_PID}" 2>/dev/null || true
        wait "${QEMU_PID}" 2>/dev/null || true
    fi
    rm -f "${LOG_FILE}"
}
trap cleanup EXIT

cd "${ROOT_DIR}"

timeout_s="${LA64_FULL_SMOKE_TIMEOUT:-20}"

make ARCH=la64 LEVEL=warn kernel-la64-full scripts/test-la64.img >/dev/null

qemu-system-loongarch64 \
    -machine virt \
    -cpu la464 \
    -nographic \
    -kernel "${ROOT_DIR}/.target-kernel/loongarch64-unknown-none/release/kernel" \
    -smp "${SMP:-4}" \
    -m 1G \
    -drive file="${ROOT_DIR}/scripts/test-la64.img",format=raw,if=none,id=hd0 \
    -device virtio-blk-pci-non-transitional,drive=hd0,rombar=0 >"${LOG_FILE}" 2>&1 &
QEMU_PID=$!

elapsed=0
while kill -0 "${QEMU_PID}" 2>/dev/null; do
    if grep -q "hello world" "${LOG_FILE}" \
        && grep -q "la64 bring-up: virtio blk initialized from platform discovery" "${LOG_FILE}" \
        && grep -q "la64 virtio blk sector0 ok" "${LOG_FILE}" \
        && grep -q "lwext4 mounted at /" "${LOG_FILE}" \
        && grep -q "delegate running" "${LOG_FILE}" \
        && grep -q "exec OK: /bin/initproc" "${LOG_FILE}" \
        && grep -q "\\[initproc\\] started pid=" "${LOG_FILE}"; then
        echo "[la64-full-smoke] PASS: boot+virtio-blk+lwext4+initproc markers observed"
        kill "${QEMU_PID}" 2>/dev/null || true
        wait "${QEMU_PID}" 2>/dev/null || true
        exit 0
    fi

    if grep -q "panic" "${LOG_FILE}"; then
        echo "[la64-full-smoke] FAIL: panic observed"
        exit 1
    fi

    sleep 1
    elapsed=$((elapsed + 1))
    if [[ "${elapsed}" -ge "${timeout_s}" ]]; then
        echo "[la64-full-smoke] FAIL: timeout (${timeout_s}s) waiting full-system markers"
        echo "--- last 60 lines ---"
        tail -n 60 "${LOG_FILE}" || true
        exit 1
    fi
done

echo "[la64-full-smoke] FAIL: qemu exited early"
echo "--- last 60 lines ---"
tail -n 60 "${LOG_FILE}" || true
exit 1
