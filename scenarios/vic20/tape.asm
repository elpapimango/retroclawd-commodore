        .cpu 6502
        ; Leave the empty BASIC program at $1001 intact after LOAD relinks it.
        .org $1200
        .include "pattern-body.asm"
