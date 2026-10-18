; Scenario program for the Commodore 64 (.prg with a BASIC stub, or on a .d64): prints HELLO through the KERNAL's CHROUT
; and returns to BASIC.
        .cpu 6502
        .org $080d
start:  ldx #0
next:   lda msg,x
        beq done
        jsr $ffd2
        inx
        bne next
done:   rts
msg:    .asciiz "HELLO"
