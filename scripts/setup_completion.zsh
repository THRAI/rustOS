# Source this file in your ~/.zshrc or directly in your terminal to enable
# auto-completion for LOG= and LEVEL= arguments in our OS kernel Makefile.

if ! type _make >/dev/null 2>&1; then
    autoload -Uz _make
fi

_freebsd_make_comp() {
    # Only supply custom arguments if we are in the kernel project
    if [[ ! -f Makefile ]] || ! grep -q "kernel-rv64" Makefile; then
        _make "$@"
        return
    fi

    # Handle LOG=... completion
    if [[ -prefix LOG= ]]; then
        local -a modules=(boot syscall trap vm sched fs driver smp signal pipe exec proc all)
        compset -P '*,' || compset -P 'LOG='
        compadd -S ',' -q -a modules
        compadd -a modules
        return 0
    fi

    # Handle LEVEL=... completion
    if [[ -prefix LEVEL= ]]; then
        local -a levels=(error warn info debug trace all)
        compset -P 'LEVEL='
        compadd -a levels
        return 0
    fi

    # Provide prefix suggestions if they haven't started typing one
    if [[ $words[CURRENT] != *"="* ]]; then
        compadd -S '' "LOG=" "LEVEL="
    fi

    # Fallback to standard make completion (for targets like run-rv64, etc.)
    _make "$@"
}

# Override the default make completion
compdef _freebsd_make_comp make
