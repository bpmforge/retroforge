; RetroForge -- RF-Scroller (ticket W2-10) -- CHR-ROM pattern tables.
;
; Original pixel art, authored for this ticket (CC0 -- see ../LICENSE).
; Every tile below is a hand-authored 2-bit NES tile: 8 bytes "plane 0"
; followed by 8 bytes "plane 1", one byte per row (bit 7 = leftmost
; pixel); the pixel's palette index is (plane1_bit<<1)|plane0_bit, so
; index 0 is transparent-on-sprites / backdrop-on-background, and indices
; 1-3 select the other three palette entries. Deliberately simple flat
; fills and a hand-plotted diamond outline -- geometric shapes, not
; "art"; see FORMAT.md's "CHR budget" section for why only 15 of the 512
; available tile slots are used.
;
; Layout note (discovered empirically while building this ticket, and
; the reason BG and sprites share ONE pattern table below rather than
; two): cc65's "nes" target crt0.o unconditionally imports `NESfont`
; (verified via `od65 --dump-imports` against nes.lib's crt0.o -- it is
; referenced by the mandatory startup code, not by any conio function
; this program calls), which pulls in nes.lib's neschar.o and its own
; 4096-byte CHARS contribution -- the built-in system font -- into this
; same segment, whether or not this program ever prints a character.
; There is no cc65-supported way to suppress that pull-in short of a
; custom crt0 (out of scope -- the ticket's toolchain notes already rule
; out vendoring a custom NES C runtime). Two 4096-byte tiles' worth of
; CHR-ROM therefore go to a font this game never uses; this file emits
; only the tiles actually needed (240 bytes, well under one 4096-byte
; pattern table) and lets the linker place the mandatory font in
; whatever space is left (nes.cfg's `fill=yes` zero-pads the true
; remainder). Consequence: BG and sprite tiles below share the SAME
; physical pattern table (main.c's PPUCTRL_BASE sets bit3=bit4=0, i.e.
; "read both BG and sprites from $0000") rather than the two-separate-
; tables layout a from-scratch NROM program without this constraint
; would use -- documented in FORMAT.md's "CHR budget" section.
;
; Layout (nes.cfg's CHARS segment = ROM2, 8KB = 512 tiles total; this
; file's own content occupies the first 240 bytes = tiles $00-$0E of
; pattern table 0, $0000-$0FFF):
;     $00        blank / sky (all-transparent, backdrop shows through)
;     $01        ground-top (flat palette index 1)
;     $02        ground-fill (flat palette index 2)
;     $03-$06    landmark A, 2x2 metatile (TL,TR,BL,BR) -- diamond outline
;     $07-$0A    landmark B, 2x2 metatile (TL,TR,BL,BR) -- checkerboard
;     $0B        HUD band, row 1 (flat palette index 3, fully opaque --
;                sprite-0's collision partner, see main.c's split logic)
;     $0C        HUD band, row 0 (flat palette index 1)
;     $0D        player sprite (flat palette index 1, fully opaque)
;     $0E        sprite-0 HUD-split sentinel (flat palette index 2, fully
;                opaque -- advisor hardening note: full-tile opacity means
;                the hit cannot depend on sub-pixel alignment)
;     $0F-$FF and all of pattern table 1 ($1000-$1FFF): the mandatory
;                NESfont (partial) plus zero fill -- never referenced by
;                this program's nametable or OAM data.

.macro FLAT1
    .byte $FF,$FF,$FF,$FF,$FF,$FF,$FF,$FF   ; plane 0: all bits set
    .byte $00,$00,$00,$00,$00,$00,$00,$00   ; plane 1: all clear -> index 1
.endmacro

.macro FLAT2
    .byte $00,$00,$00,$00,$00,$00,$00,$00   ; plane 0: all clear
    .byte $FF,$FF,$FF,$FF,$FF,$FF,$FF,$FF   ; plane 1: all set -> index 2
.endmacro

.macro FLAT3
    .byte $FF,$FF,$FF,$FF,$FF,$FF,$FF,$FF   ; plane 0: all set
    .byte $FF,$FF,$FF,$FF,$FF,$FF,$FF,$FF   ; plane 1: all set -> index 3
.endmacro

.macro BLANK
    .byte $00,$00,$00,$00,$00,$00,$00,$00
    .byte $00,$00,$00,$00,$00,$00,$00,$00
.endmacro

.segment "CHARS"

; ---- Background pattern table ($0000-$0FFF) ----

; $00 blank/sky
BLANK

; $01 ground-top
FLAT1

; $02 ground-fill
FLAT2

; $03 landmark A, top-left quadrant of a hand-plotted diamond outline
; (palette index 3 on transparent field; both planes carry the identical
; bit pattern below, per the index-3 = both-planes-set rule above).
    .byte $00,$00,$00,$01,$03,$07,$0F,$1F
    .byte $00,$00,$00,$01,$03,$07,$0F,$1F
; $04 landmark A, top-right quadrant (mirror of $03)
    .byte $00,$00,$00,$80,$C0,$E0,$F0,$F8
    .byte $00,$00,$00,$80,$C0,$E0,$F0,$F8
; $05 landmark A, bottom-left quadrant (vertical mirror of $03)
    .byte $3F,$1F,$0F,$07,$03,$01,$00,$00
    .byte $3F,$1F,$0F,$07,$03,$01,$00,$00
; $06 landmark A, bottom-right quadrant (vertical mirror of $04)
    .byte $FC,$F8,$F0,$E0,$C0,$80,$00,$00
    .byte $FC,$F8,$F0,$E0,$C0,$80,$00,$00

; $07 landmark B top-left: flat index 2
FLAT2
; $08 landmark B top-right: flat index 1 (checkerboard against $07)
FLAT1
; $09 landmark B bottom-left: flat index 1
FLAT1
; $0A landmark B bottom-right: flat index 2 (checkerboard against $09)
FLAT2

; $0B HUD band row 1 -- fully opaque (sprite-0's BG collision partner)
FLAT3

; $0C HUD band row 0
FLAT1

; $0D player -- flat index 1, fully opaque (shares this pattern table
; with the BG tiles above -- see this file's module doc)
FLAT1

; $0E sprite-0 HUD-split sentinel -- flat index 2, fully opaque
FLAT2

; No trailing .res here: this segment's content ends at 240 bytes
; ($00-$0E, 15 tiles). The mandatory NESfont (this file's module doc)
; and nes.cfg's own `fill=yes` account for the rest of ROM2's declared
; 8192 bytes -- padding it again here would double-count and overflow
; the memory area, which is exactly the build error this layout fixes.
