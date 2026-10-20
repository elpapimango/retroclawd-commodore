        sei
        lda #12
        sta $9000
        lda #38
        sta $9001
        lda #0
        sta $9005
        lda #1
        sta $9002
        sta $0000
        lda #2
        sta $9003
        sta $9400
        lda #$80
        sta $9005
        lda #$1B
        sta $900F
        lda #$E0
        sta $900A
        lda #15
        sta $900E
loop:   jmp loop
