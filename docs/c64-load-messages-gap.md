# C64: a fast-loaded disk prints no SEARCHING / LOADING lines (S3-d64, closed 2026-10-19)

**Closed 2026-10-19.** After a successful disk load the trap no longer returns straight to the caller: it pushes two return
addresses and jumps into the KERNAL's own `$F5AF` (SEARCHING FOR <name>), which returns into `$F5D2` (LOADING, or VERIFYING
from `$93`), which returns into `$F5A9` (LOAD's exit: `CLC`, `LDX $AE`, `LDY $AF`, `RTS`) and so to the caller. All three are
checked byte for byte in the KERNAL in place first (901227-03 bytes confirmed); a custom KERNAL without them gets the old
direct return. The routines print only in direct mode (`$9D` bit 7), as on the machine. Test
`tests/disk_save.rs::a_fast_disk_load_prints_the_kernals_searching_and_loading_lines` (real ROMs, mutation-checked); S3-d64
passes. A file-not-found still returns without SEARCHING FOR; tape loads are unchanged.

The original finding:

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
