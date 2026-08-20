; RetroForge -- RF-Scroller-S (ticket W6-05, D-001, FR-CORE-037).
;
; The project's own in-repo SNES fixture: game-shaped content we control
; end to end, built from source in CI, never a fetched binary.
;
; WHAT IT DOES
;   A horizontally scrolling playfield in BG mode 1. The D-pad moves a
;   player marker; the camera follows it; new tile columns are streamed
;   into the tilemap as the camera advances, so the 32-tile-wide map is
;   reused indefinitely rather than the level being one screen wide.
;
; WHY THE STREAMING MATTERS (and why it is the anti-vacuity hook)
;   A fixture that only ever renders its initial screen would pass a
;   replay hash forever while proving nothing about scrolling. Column
;   streaming means a scripted input log that actually holds Right long
;   enough will REUSE physical tilemap columns for new content, and the
;   harness asserts a `columns_streamed` value that is only reachable
;   once that has happened. This mirrors RF-Scroller (NES), whose
;   FORMAT.md records the same trap and the defect it once hid.
;
; RAM MAP -- these addresses are documented in FORMAT.md and read by
; crates/rf-harness/tests/rf_scroller_s_replay.rs. DECLARATION ORDER
; DECIDES LAYOUT: anything new goes at the END, or every documented
; address silently goes stale (RF-Scroller learned this the hard way in
; W5-02c).

.p816
.smart

.segment "ZEROPAGE"
frame_counter:    .res 2   ; $00 frames since reset
player_x:         .res 2   ; $02 player position, in pixels
camera_x:         .res 2   ; $04 camera scroll, in pixels
columns_streamed: .res 2   ; $06 tile columns written since reset
next_column:      .res 2   ; $08 next world column to stream
pad1:             .res 2   ; $0A joypad 1, latched
scratch:          .res 2   ; $0C scratch for address arithmetic
ground_top:       .res 2   ; $0E first ground row of the column being written

.segment "CODE"

reset:
    sei
    clc
    xce                     ; leave emulation mode
    rep #$30
    .a16
    .i16
    ldx #$1FFF
    txs

    jsr init_regs
    jsr init_palette
    jsr init_tiles
    jsr init_state
    jsr preload_columns

    sep #$20
    .a8
    lda #$0F
    sta $2100               ; screen on, full brightness
    rep #$20
    .a16

main_loop:
    jsr wait_vblank
    jsr read_pad
    jsr move_player
    jsr update_camera
    jsr stream_if_needed
    jsr apply_scroll
    inc frame_counter
    bra main_loop

; ---------------------------------------------------------------------

init_regs:
    sep #$20
    .a8
    lda #$8F
    sta $2100               ; forced blank while we set up
    stz $2105               ; BG mode 0 (four 2bpp layers)
    stz $2107               ; BG1 tilemap at word 0, 32x32
    lda #$01
    sta $210B               ; BG1 characters at word $1000
    lda #$80
    sta $2115               ; VMAIN: increment after the high byte
    lda #$01
    sta $212C               ; BG1 on the main screen
    lda #$01
    sta $4200               ; auto-joypad read on, no NMI
    rep #$20
    .a16
    rts

init_palette:
    sep #$20
    .a8
    stz $2121               ; CGRAM address 0
    ; colour 0: black backdrop
    stz $2122
    stz $2122
    ; colour 1: mid blue  (BGR555 $7C00 = blue)
    stz $2122
    lda #$7C
    sta $2122
    ; colour 2: green
    lda #$E0
    sta $2122
    lda #$03
    sta $2122
    ; colour 3: white
    lda #$FF
    sta $2122
    lda #$7F
    sta $2122
    rep #$20
    .a16
    rts

; Four 2bpp characters, generated rather than pulled from an asset file:
;   0 = empty, 1 = solid colour 1, 2 = solid colour 2, 3 = solid colour 3.
; Keeping the art in-source is deliberate -- a fixture whose graphics live
; in a binary blob is a fixture whose build is not reproducible from text.
init_tiles:
    sep #$20
    .a8
    ; VRAM word address $1000.
    stz $2116
    lda #$10
    sta $2117
    rep #$20
    .a16

    ldx #$0000              ; character 0: all zero -> transparent
    ldy #8
@tile0:
    stz $2118
    dey
    bne @tile0

    ldy #8                  ; character 1: plane 0 set -> colour 1
@tile1:
    sep #$20
    .a8
    lda #$FF
    sta $2118
    stz $2119
    rep #$20
    .a16
    dey
    bne @tile1

    ldy #8                  ; character 2: plane 1 set -> colour 2
@tile2:
    sep #$20
    .a8
    stz $2118
    lda #$FF
    sta $2119
    rep #$20
    .a16
    dey
    bne @tile2

    ldy #8                  ; character 3: both planes -> colour 3
@tile3:
    sep #$20
    .a8
    lda #$FF
    sta $2118
    sta $2119
    rep #$20
    .a16
    dey
    bne @tile3
    rts

init_state:
    stz frame_counter
    stz camera_x
    stz columns_streamed
    stz next_column
    lda #16
    sta player_x
    rts

; Fill the whole 32-column tilemap before the first frame, so the screen
; is never partially drawn.
preload_columns:
    ldx #32
@loop:
    jsr stream_one_column
    dex
    bne @loop
    rts

; ---------------------------------------------------------------------
; Streaming: write one 28-tile-tall column into the tilemap.
;
; The column's CONTENT is a function of its world index, so the level is
; deterministic and infinite without any stored level data: a ground band
; at the bottom whose height varies with the column number, and a marker
; tile every eighth column so scrolling is visible.
stream_one_column:
    php
    rep #$30
    .a16
    .i16
    phx
    phy
    ; PHX/PHY are not optional. PHP saves the FLAGS, not the registers,
    ; and this routine uses X as its row counter -- the first version
    ; clobbered preload_columns' loop counter and streamed 30000 columns
    ; without ever leaving forced blank.

    ; Ground starts at row (24 - (column mod 4)), so the surface varies
    ; and horizontal motion is visible without any stored level data.
    lda next_column
    and #$0003
    sta scratch
    lda #24
    sec
    sbc scratch
    sta ground_top

    ldx #0                  ; x = row
@row:
    ; Tilemap word address for (row, col) = row*32 + (col mod 32).
    txa
    asl
    asl
    asl
    asl
    asl
    sta scratch
    lda next_column
    and #$001F
    clc
    adc scratch
    and #$03FF
    sta $2116

    cpx ground_top
    bcc @sky
    lda #$0002              ; ground
    bra @write
@sky:
    lda #$0000              ; sky -> backdrop
@write:
    sta $2118

    inx
    cpx #28
    bne @row

    inc next_column
    inc columns_streamed
    ply
    plx
    plp
    rts

; ---------------------------------------------------------------------

wait_vblank:
    sep #$20
    .a8
@w1:
    lda $4210
    bmi @w1
@w2:
    lda $4210
    bpl @w2
    rep #$20
    .a16
    rts

read_pad:
    sep #$20
    .a8
@busy:
    lda $4212
    and #$01
    bne @busy               ; wait for auto-joypad to finish
    rep #$20
    .a16
    lda $4218
    sta pad1
    rts

; Right (bit 8 of the high byte -> $0100) moves forward, Left back.
move_player:
    lda pad1
    and #$0100
    beq @check_left
    lda player_x
    inc
    sta player_x
    bra @done
@check_left:
    lda pad1
    and #$0200
    beq @done
    lda player_x
    beq @done
    dec
    sta player_x
@done:
    rts

; The camera follows the player, held one half-screen behind.
update_camera:
    lda player_x
    cmp #128
    bcc @zero
    sec
    sbc #128
    sta camera_x
    rts
@zero:
    stz camera_x
    rts

; Stream a new column whenever the camera has advanced past the last one
; we wrote. This is what makes the tilemap get REUSED.
stream_if_needed:
    lda camera_x
    lsr
    lsr
    lsr                     ; camera in tiles
    clc
    adc #32                 ; one screen ahead
    cmp next_column
    bcc @done
    jsr stream_one_column
@done:
    rts

apply_scroll:
    sep #$20
    .a8
    lda camera_x
    sta $210D               ; BG1HOFS low
    rep #$20
    .a16
    lda camera_x
    xba
    sep #$20
    .a8
    sta $210D               ; BG1HOFS high
    rep #$20
    .a16
    rts

; ---------------------------------------------------------------------

.segment "SNESHDR"
    .byte "RF-SCROLLER-S        "  ; 21 chars
    .byte $20                      ; LoROM, SlowROM
    .byte $00                      ; ROM only
    .byte $08                      ; 1<<8 KB = 256 KB declared
    .byte $00                      ; no SRAM
    .byte $00, $00, $00
    .word $0000, $FFFF             ; checksum + complement

.segment "VECTORS"
    .word 0, 0, 0, 0, 0, 0, 0, 0
    .word 0, 0, 0, 0, 0, 0, reset, 0
