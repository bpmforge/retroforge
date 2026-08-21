; RetroForge -- Action 53 (iNES mapper 28) conformance fixture (W7-11).
;
; WHY THIS EXISTS
;
; W7-11's third criterion asks for "test ROMs for both mappers". There is
; no published mapper-28 conformance ROM: Action 53 is a homebrew
; multicart collection, not a mapper with a test suite. Rather than drop
; the criterion, this fixture is built from this repo's own cc65
; toolchain, the same way fixtures/nes/rf-scroller is.
;
; WHAT IT PROVES
;
; The banking arithmetic, which is the part of mapper 28 that is not
; guessable. Each of the eight 16 KiB banks is stamped with its own index
; at its first byte. The test selects each bank in turn through the
; $5000 select latch + $8000 data port, reads the stamp back, and records
; a pass/fail witness.
;
; It also proves the two details that separate mapper 28 from plain UNROM
; (see the mapper's own doc): the $5000 select latch outside $8000-$FFFF,
; and mode 3's FIXED top half, which must stay put while $8000 moves.

.segment "HEADER"
    .byte "NES", $1A
    .byte 8              ; 8 x 16 KiB PRG = 128 KiB
    .byte 0              ; CHR RAM
    .byte $C0            ; flags 6: mapper low nibble 12? -> see flags 7
    .byte $10            ; flags 7: mapper high nibble 1 -> mapper 28
    .byte 0, 0, 0, 0, 0, 0, 0, 0

; ---------------------------------------------------------------------
; Seven switchable banks, each stamped with its own index.
; ---------------------------------------------------------------------
.macro STAMPED_BANK n
    .byte n
    .res  $3FFF, $FF
.endmacro

.segment "BANK0"
    STAMPED_BANK 0
.segment "BANK1"
    STAMPED_BANK 1
.segment "BANK2"
    STAMPED_BANK 2
.segment "BANK3"
    STAMPED_BANK 3
.segment "BANK4"
    STAMPED_BANK 4
.segment "BANK5"
    STAMPED_BANK 5
.segment "BANK6"
    STAMPED_BANK 6

; ---------------------------------------------------------------------
; Bank 7 (the LAST bank) holds the code and the vectors.
;
; Not an arbitrary choice: nesdev specifies mapper 28 powers up with the
; PRG mode bits set and outer = $3F, so $C000 shows (outer << 1) | 1 =
; 127, which is bank 7 of eight. The reset vector has to be in whatever
; bank that lands on, and the test then keeps it there by setting
; outer = 3 -- (3 << 1) | 1 = 7 -- so the fixed half never moves while
; $8000 does.
; ---------------------------------------------------------------------
.segment "FIXED"
    .byte 7                       ; this bank's own stamp

reset:
    sei
    cld
    ldx #$FF
    txs

    ; Witness starts as "running" ($80), blargg-style.
    lda #$80
    sta $0200
    sta $6000

    ; mode := prg_mode 3, outer size 3, mirroring 2 (vertical).
    ;   bits 5-4 = 11 (size 3 -> 4 inner bits)
    ;   bits 3-2 = 11 (mode 3: switchable $8000, fixed $C000)
    ;   bits 1-0 = 10 (vertical, so D4 is ignored on $00/$01 writes)
    lda #$80
    sta $5000
    lda #$3E
    sta $8000

    ; outer := 3, so the fixed half stays on bank 7 where this code is:
    ; (3 << 1) | 1 = 7. With 4 inner bits, $8000 = (3 << 4) | inner = 48 +
    ; inner, and 48 is a multiple of the 8-bank count, so $8000 shows
    ; exactly `inner`.
    lda #$81
    sta $5000
    lda #$03
    sta $8000

    ldx #0                        ; bank index under test
test_loop:
    ; inner := X, through the $01 register.
    lda #$01
    sta $5000
    txa
    sta $8000

    ; The stamp at $8000 must equal the bank we asked for.
    lda $8000
    stx $0203                     ; record which bank failed, if any
    cmp $0203
    bne fail

    ; And the FIXED bank must not have moved: $C000 still stamps 7.
    lda $C000
    cmp #7
    bne fail

    inx
    cpx #8
    bne test_loop

pass:
    lda #0
    sta $0200
    sta $6000
    jmp done

fail:
    lda #1
    sta $0200
    sta $6000

done:
    ; blargg signature, so a $6000-protocol reader recognises the result.
    lda #$DE
    sta $6001
    lda #$B0
    sta $6002
    lda #$61
    sta $6003
forever:
    jmp forever

nmi:
irq:
    rti

.segment "VECTORS"
    .word nmi
    .word reset
    .word irq
