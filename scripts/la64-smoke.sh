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

timeout_s="${LA64_SMOKE_TIMEOUT:-15}"

if [[ ! -f "${ROOT_DIR}/.target-kernel/loongarch64-unknown-none/release/kernel" ]]; then
    make ARCH=la64 kernel-la64 >/dev/null
fi

qemu-system-loongarch64 \
    -machine virt \
    -cpu la464 \
    -nographic \
    -kernel "${ROOT_DIR}/.target-kernel/loongarch64-unknown-none/release/kernel" \
    -smp "${SMP:-4}" \
    -m 1G >"${LOG_FILE}" 2>&1 &
QEMU_PID=$!

elapsed=0
while kill -0 "${QEMU_PID}" 2>/dev/null; do
    if grep -q "hello world" "${LOG_FILE}" \
        && grep -q "la64 kernel-only bring-up: skipping fs/init/userland path" "${LOG_FILE}" \
        && grep -q "la64 irq save/restore self-check PASS" "${LOG_FILE}" \
        && grep -q "la64 timer handler entered" "${LOG_FILE}" \
        && grep -q "la64 mmu status:" "${LOG_FILE}"; then
        echo "[la64-smoke] PASS: boot+irq+timer+mmu markers observed"
        kill "${QEMU_PID}" 2>/dev/null || true
        wait "${QEMU_PID}" 2>/dev/null || true
        exit 0
    fi

    if grep -q "panic" "${LOG_FILE}"; then
        echo "[la64-smoke] FAIL: panic observed"
        exit 1
    fi

    sleep 1
    elapsed=$((elapsed + 1))
    if [[ "${elapsed}" -ge "${timeout_s}" ]]; then
        echo "[la64-smoke] FAIL: timeout (${timeout_s}s) waiting boot markers"
        echo "--- last 40 lines ---"
        tail -n 40 "${LOG_FILE}" || true
        exit 1
    fi
done

echo "[la64-smoke] FAIL: qemu exited early"
echo "--- last 40 lines ---"
tail -n 40 "${LOG_FILE}" || true
exit 1
