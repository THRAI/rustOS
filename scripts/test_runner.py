#!/usr/bin/env python3
"""QEMU Integration Test Runner.

Boots the kernel in QEMU, waits for a shell prompt, then sends commands from a
file one at a time and expects the prompt to return after each.  Designed to run
both interactively on a developer machine and unattended in CI.
"""

import sys
import signal
import pexpect
import argparse
import os
import time


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def sendline_throttled(child, text, char_delay):
    """Send a command to UART one byte at a time to avoid RX FIFO overruns."""
    for ch in text:
        child.send(ch)
        if char_delay > 0:
            time.sleep(char_delay)
    child.send("\n")


def _kill_child(child):
    """Best-effort QEMU cleanup."""
    if child is None or not child.isalive():
        return
    try:
        child.kill(signal.SIGTERM)
        child.wait()
    except Exception:
        try:
            child.kill(signal.SIGKILL)
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description="QEMU Integration Test Runner")
    parser.add_argument(
        "--interactive",
        action="store_true",
        help="Run in interactive mode (drop to shell)",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=60,
        help="Timeout in seconds for each expect (default: 60)",
    )
    parser.add_argument(
        "--char-delay",
        type=float,
        default=0.003,
        help="Delay between each sent character in script mode",
    )
    parser.add_argument(
        "--cmd-file",
        default="scripts/intergration_command",
        help="Path to the file containing test commands",
    )
    parser.add_argument("qemu_cmd", help="The QEMU executable")
    parser.add_argument(
        "qemu_args", nargs=argparse.REMAINDER, help="Arguments passed to QEMU"
    )

    args = parser.parse_args()

    # The arg list is often passed with a '--' to separate qemu args
    if args.qemu_args and args.qemu_args[0] == "--":
        args.qemu_args = args.qemu_args[1:]

    qemu_full_command = [args.qemu_cmd] + args.qemu_args
    print(f"=== Starting QEMU Integration Test ===")
    print(f"Command: {' '.join(qemu_full_command)}")

    if args.interactive:
        print("=== Running in INTERACTIVE mode ===")
        os.execvp(args.qemu_cmd, qemu_full_command)
    else:
        print("=== Running in SCRIPT mode ===")

        commands = []
        if os.path.exists(args.cmd_file):
            with open(args.cmd_file, "r") as f:
                commands = [
                    line.strip()
                    for line in f
                    if line.strip() and not line.startswith("#")
                ]
            print(f"[*] Loaded {len(commands)} commands from {args.cmd_file}")
        else:
            print(
                f"[!] Warning: Command file {args.cmd_file} not found. "
                "Running with no commands."
            )

        run_script_test(
            args.qemu_cmd,
            args.qemu_args,
            args.timeout,
            commands,
            args.char_delay,
        )


# ---------------------------------------------------------------------------
# Script-mode test driver
# ---------------------------------------------------------------------------


def run_script_test(qemu_cmd, qemu_args, timeout, commands, char_delay):
    child = pexpect.spawn(qemu_cmd, args=qemu_args, encoding="utf-8", timeout=timeout)
    child.logfile = sys.stdout

    # Track results: list of (command, status, duration_secs)
    results = []
    total = len(commands)

    try:
        # ---- Boot ----
        boot_start = time.monotonic()
        print("\n[*] Waiting for boot sequence to complete and shell prompt...")
        child.expect(r"# ", timeout=timeout)
        boot_dur = time.monotonic() - boot_start
        print(f"\n[*] Boot completed in {boot_dur:.1f}s")

        # ---- Run each command ----
        for idx, cmd in enumerate(commands, 1):
            label = f"[{idx}/{total}]"
            print(f"\n\n{label} Testing: {cmd}")

            # Drain residual output
            while True:
                try:
                    child.read_nonblocking(size=4096, timeout=0.1)
                except (pexpect.TIMEOUT, pexpect.EOF):
                    break

            cmd_start = time.monotonic()
            sendline_throttled(child, cmd, char_delay)

            try:
                child.expect(r"# ", timeout=timeout)
                dur = time.monotonic() - cmd_start
                results.append((cmd, "PASS", dur))
                print(f"\n{label} PASS ({dur:.2f}s)")
            except pexpect.TIMEOUT:
                dur = time.monotonic() - cmd_start
                results.append((cmd, "TIMEOUT", dur))
                print(f"\n{label} FAIL (timeout after {dur:.1f}s)")
                # Try to re-sync with prompt; if we can't, abort
                try:
                    child.expect(r"# ", timeout=5)
                except (pexpect.TIMEOUT, pexpect.EOF):
                    for remaining_cmd in commands[idx:]:
                        results.append((remaining_cmd, "SKIPPED", 0.0))
                    break
            except pexpect.EOF:
                dur = time.monotonic() - cmd_start
                results.append((cmd, "QEMU_EXIT", dur))
                for remaining_cmd in commands[idx:]:
                    results.append((remaining_cmd, "SKIPPED", 0.0))
                break

    except pexpect.TIMEOUT:
        print("\n\n=== TEST FAILED: Timeout waiting for initial shell prompt ===")
        _kill_child(child)
        sys.exit(1)
    except pexpect.EOF:
        print("\n\n=== TEST FAILED: QEMU exited before shell prompt appeared ===")
        sys.exit(1)
    finally:
        _kill_child(child)

    # ---- Summary ----
    passed = sum(1 for _, s, _ in results if s == "PASS")
    failed = sum(1 for _, s, _ in results if s not in ("PASS", "SKIPPED"))
    skipped = sum(1 for _, s, _ in results if s == "SKIPPED")

    print("\n")
    print("=" * 72)
    print("  INTEGRATION TEST SUMMARY")
    print("=" * 72)
    print(f"  {'#':<4} {'Status':<10} {'Time':>8}  Command")
    print("-" * 72)
    for i, (cmd, status, dur) in enumerate(results, 1):
        time_str = f"{dur:.2f}s" if dur > 0 else "-"
        print(f"  {i:<4} {status:<10} {time_str:>8}  {cmd}")
    print("-" * 72)
    total_time = sum(d for _, _, d in results)
    print(
        f"  {passed} passed, {failed} failed, {skipped} skipped "
        f"({total_time:.1f}s total)"
    )
    print("=" * 72)

    if failed > 0:
        print("\n=== INTEGRATION TESTS FAILED ===")
        sys.exit(1)
    else:
        print("\n=== All Integration Tests Passed! ===")
        sys.exit(0)


if __name__ == "__main__":
    main()
