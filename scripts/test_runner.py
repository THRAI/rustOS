#!/usr/bin/env python3
import sys
import pexpect
import argparse
import os
import time


def sendline_throttled(child, text, char_delay):
    """Send a command to UART one byte at a time to avoid RX FIFO overruns."""
    for ch in text:
        child.send(ch)
        if char_delay > 0:
            time.sleep(char_delay)
    child.send('\n')

def main():
    parser = argparse.ArgumentParser(description="QEMU Integration Test Runner")
    parser.add_argument('--interactive', action='store_true', help="Run in interactive mode (drop to shell)")
    parser.add_argument('--timeout', type=int, default=30, help="Timeout for expect in script mode")
    parser.add_argument(
        '--char-delay',
        type=float,
        default=0.003,
        help="Delay between each sent character in script mode",
    )
    parser.add_argument('--cmd-file', default='scripts/intergration_command', help="Path to the file containing test commands")
    parser.add_argument('qemu_cmd', help="The QEMU executable")
    parser.add_argument('qemu_args', nargs=argparse.REMAINDER, help="Arguments passed to QEMU")

    args = parser.parse_args()

    # The arg list is often passed with a '--' to separate qemu args, remove it if present
    if args.qemu_args and args.qemu_args[0] == '--':
        args.qemu_args = args.qemu_args[1:]

    qemu_full_command = [args.qemu_cmd] + args.qemu_args
    print(f"=== Starting QEMU Integration Test ===")
    print(f"Command: {' '.join(qemu_full_command)}")

    if args.interactive:
        print("=== Running in INTERACTIVE mode ===")
        # Using os.execvp allows QEMU to take over the terminal process entirely
        # It's an exact replacement for running it directly from the Makefile
        os.execvp(args.qemu_cmd, qemu_full_command)
    else:
        print("=== Running in SCRIPT mode ===")

        commands = []
        if os.path.exists(args.cmd_file):
            with open(args.cmd_file, 'r') as f:
                commands = [line.strip() for line in f if line.strip() and not line.startswith('#')]
            print(f"[*] Loaded {len(commands)} commands from {args.cmd_file}")
        else:
            print(f"[!] Warning: Command file {args.cmd_file} not found. Running with no commands.")

        run_script_test(
            args.qemu_cmd,
            args.qemu_args,
            args.timeout,
            commands,
            args.char_delay,
        )


def run_script_test(qemu_cmd, qemu_args, timeout, commands, char_delay):
    # Spawn the QEMU process.
    # Log everything to stdout so we can watch it happen live
    child = pexpect.spawn(qemu_cmd, args=qemu_args, encoding='utf-8', timeout=timeout)
    child.logfile = sys.stdout

    try:
        # 1. Wait for OS to boot and drop into the shell
        print("\n[*] Waiting for boot sequence to complete and shell prompt...")
        # Assume the shell prompt ends with '# ' (adjust if your init drops to different prompt)
        child.expect(r'# ', timeout=timeout)

        for cmd in commands:
            print(f"\n\n[+] Testing command: '{cmd}'")
            # Clear out any noisy log buffers that were unread before sending the command
            while True:
                try:
                    child.read_nonblocking(size=4096, timeout=0.1)
                except pexpect.TIMEOUT:
                    break
                except pexpect.EOF:
                    break

            # Send the command
            sendline_throttled(child, cmd, char_delay)

            # Since LOG=all creates extreme spam, we just wait for the prompt to return.
            # We don't assert the command was cleanly echoed, because kernel logs will interleave.
            child.expect(r'# ', timeout=timeout)

            # Note: We aren't doing strict output validation here because with LOG=all,
            # the output is entirely garbled with kernel debugs. If QEMU didn't crash
            # and the Prompt survived, we consider the command "executed".

        print("\n\n=== All Python Integration Tests Passed! ===")
        sys.exit(0)

    except pexpect.TIMEOUT:
        print("\n\n=== TEST FAILED: Timeout waiting for expected output ===")
        sys.exit(1)
    except pexpect.EOF:
        print("\n\n=== TEST FAILED: QEMU exited unexpectedly ===")
        sys.exit(1)

if __name__ == '__main__':
    main()
