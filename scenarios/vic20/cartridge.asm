        .cpu 6502
        .org $A000
        .word start, start
        ; The real VIC-20 KERNAL checks this signature at $A004.
        .byte $41, $30, $C3, $C2, $CD
start:
        .include "pattern-body.asm"
        .fill $C000 - *, $FF
