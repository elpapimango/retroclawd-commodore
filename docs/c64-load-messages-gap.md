# C64: a fast-loaded disk prints no SEARCHING / LOADING lines (S3-d64 known gap)

Found 2026-10-18 by `c64.manifest.toml` scenario S3-d64.

VICE 3.10 autostarting `c64-hello.d64` (made by `rcld`, `scenarios/c64/hello.asm`) shows, from the top:

    READY.
    LOAD"*",8,1

    SEARCHING FOR *
    LOADING
    READY.
    RUN
    HELLO
    READY.

retroclawd, typing the same `LOAD"*",8,1` and `RUN` with `--disk` (the fast-load trap `C64::try_fast_kernal_load_trap`):

    READY.
    LOAD"*",8,1

    READY.
    RUN
    HELLO
    READY.

The program runs and prints HELLO in both. The two lines a real KERNAL LOAD prints (SEARCHING FOR, LOADING) are missing, because
the trap replaces the whole routine. The picture differs from the golden by 3.42 (limit 1). Next step: have the trap print the
two messages (the KERNAL routines that print them; look the addresses up in the committed KERNAL), or accept the difference
and say so in the manifest.
