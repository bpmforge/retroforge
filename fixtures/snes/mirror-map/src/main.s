; RetroForge -- SNES LoROM/HiROM mirror-map fixture (ticket W6-06).
;
; WHAT THIS PROVES, and why it is a fixture rather than a unit test:
; cartridge address mapping is a property of the BUS, and the only way to
; assert it end to end is to have real code observe it from inside the
; machine. This ROM reads the same byte through every address a correct
; LoROM mapping aliases together, and writes a pass/fail result block into
; WRAM that the harness reads -- the "golden RAM result block" protocol
; docs/TESTING.md section 5 specifies for this row (FR-CORE-035).
;
; It deliberately does NOT depend on the PPU, the APU, DMA or interrupts:
; a mapping test that needed a working PPU could not run until the PPU
; worked, which is precisely backwards for the first SNES fixture in the
; project.
;
; RESULT BLOCK, at $7E0000 (WRAM bank 0, offset 0):
;   $00  magic  $52 ('R')  -- written LAST, so a partial run is visible
;   $01  magic  $46 ('F')
;   $02  total checks run
;   $03  checks passed
;   $04.. per-check result, 1 = pass, 0 = fail
;
; The magic is written last on purpose: a harness that found it present
; knows every earlier byte was written by a run that reached the end,
; rather than reading uninitialised WRAM that happened to look plausible.

.p816
.smart

RESULT   = $000000        ; via bank $7E (WRAM), see DP/DB setup below
CHECKS   = 4

.segment "CODE"

reset:
    sei
    clc
    xce                    ; leave 6502 emulation mode -> native 65816
    rep #$30               ; 16-bit A and X/Y
    .a16
    .i16
    ldx #$1FFF
    txs                    ; stack in bank 0 RAM

    sep #$20               ; 8-bit A, 16-bit index
    .a8

    ; Data bank -> $7E so absolute stores land in WRAM.
    lda #$7E
    pha
    plb

    ldx #$0000
    ; Zero the result block first, so a check that never runs reads 0
    ; (fail) rather than whatever WRAM powered up as.
    lda #$00
clear:
    sta RESULT,x
    inx
    cpx #$0010
    bne clear

    ; ---- check 0: the ROM is readable at all ------------------------
    ; The first opcode of `reset` (SEI = $78), read through the mapping's
    ; canonical bank.
.ifdef HIROM
    lda $C08000
.else
    lda $808000
.endif
    cmp #$78
    beq c0_pass
    lda #$00
    bra c0_store
c0_pass:
    lda #$01
c0_store:
    sta RESULT+4

    ; ---- check 1: the mapping's own bank alias ----------------------
    ; LoROM aliases $00:8000 to $80:8000. HiROM aliases $40:8000 to
    ; $C0:8000. Same question -- "does the bus mirror the ROM into the
    ; low banks?" -- asked at the address each mapping actually uses.
.ifdef HIROM
    lda f:$408000
    cmp f:$C08000
.else
    ; `f:` is REQUIRED here, and its absence was a real bug (found by
    ; W6-02a, the first ticket able to run this ROM). Without it ca65 sees
    ; a bank of $00, shortens `lda $008000` to 2-byte ABSOLUTE `ad 00 80`,
    ; and absolute addressing resolves through DBR -- which this ROM set
    ; to $7E for its result-block stores. The check then read WRAM
    ; $7E:8000 and compared it against ROM $80:8000, so it could only ever
    ; fail, and when it "passed" it would have been proving nothing about
    ; the alias it names.
    ;
    ; HiROM never showed this: its operands are banks $40/$C0, so ca65 had
    ; no short form to choose and emitted long for both. An asymmetry
    ; between the two images that comes from the ASSEMBLER rather than the
    ; mapping is exactly the kind of thing "one source, two mappings" is
    ; supposed to rule out, and it slipped through because nothing could
    ; execute the pair yet.
    lda f:$008000
    cmp f:$808000
.endif
    beq c1_pass
    lda #$00
    bra c1_store
c1_pass:
    lda #$01
c1_store:
    sta RESULT+5

    ; ---- check 2: WRAM is mirrored into bank $00's low 8 KiB ---------
    ; $7E:0000-1FFF is mirrored at $00:0000-1FFF on real hardware. Write
    ; through one view, read through the other.
    lda #$A5
    sta $7E1234
    lda $001234
    cmp #$A5
    beq c2_pass
    lda #$00
    bra c2_store
c2_pass:
    lda #$01
c2_store:
    sta RESULT+6

    ; ---- check 3: the write above did not alias into ROM space ------
    ; A bus that mapped WRAM over the whole bank would corrupt the ROM
    ; read from check 0. Re-read it.
.ifdef HIROM
    lda $C08000
.else
    lda $808000
.endif
    cmp #$78
    beq c3_pass
    lda #$00
    bra c3_store
c3_pass:
    lda #$01
c3_store:
    sta RESULT+7

    ; ---- tally ------------------------------------------------------
    lda #CHECKS
    sta RESULT+2
    lda #$00
    sta RESULT+3
    ldx #$0000
tally:
    lda RESULT+4,x
    beq tally_next
    lda RESULT+3
    inc a
    sta RESULT+3
tally_next:
    inx
    cpx #CHECKS
    bne tally

    ; Magic LAST: its presence certifies the run reached the end.
    lda #$52               ; 'R'
    sta RESULT+0
    lda #$46               ; 'F'
    sta RESULT+1

done:
    bra done

; Unused vectors point here rather than at zero, so a spurious interrupt
; parks visibly instead of executing whatever fill byte happens to be at
; $000000.
stub:
    rti

.segment "SNESHDR"
.ifdef HIROM
    .byte "RF MIRROR-MAP HIROM  "   ; 21 bytes, title
.else
    .byte "RF MIRROR-MAP LOROM  "   ; 21 bytes, title
.endif
    ; Map mode: $20 = LoROM slow, $21 = HiROM slow. Set by the build
    ; script with -D HIROM, so ONE source produces both fixtures and the
    ; two cannot drift apart in anything except the mapping they test.
.ifdef HIROM
    .byte $21
.else
    .byte $20
.endif
    .byte $00                       ; cartridge type: ROM only
.ifdef HIROM
    .byte $06                       ; ROM size: 64 KiB (1<<6 KiB)
.else
    .byte $05                       ; ROM size: 32 KiB (1<<5 KiB)
.endif
    .byte $00                       ; RAM size: none
    .byte $01                       ; country
    .byte $00                       ; developer
    .byte $00                       ; version
    .word $0000                     ; checksum complement (unused here)
    .word $0000                     ; checksum (unused here)

.segment "VECTORS"
    ; Native mode
    .word stub, stub, stub, stub, stub, stub, stub, stub
    ; Emulation mode: only RESET matters for a fixture that runs from
    ; power-on and never returns to emulation mode.
    .word stub, stub, stub, stub, stub, stub
    .word reset
    .word stub
