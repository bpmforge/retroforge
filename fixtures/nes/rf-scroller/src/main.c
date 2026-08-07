/* RetroForge -- RF-Scroller (ticket W2-10): in-repo NES fixture platformer.
 *
 * cc65's OWN "nes" target (`cl65 -t nes`), not neslib -- see ../FORMAT.md
 * and the ticket report for why. No NMI vector is used anywhere in this
 * program: every frame is paced by busy-polling PPUSTATUS bit 7, the
 * exact technique the conductor's pre-flight probe verified end-to-end
 * through the real core before this game was written.
 *
 * Gameplay: the player walks right (D-pad Right only -- see FORMAT.md's
 * "What this ticket's runtime does NOT do" for why there is no Left/
 * jump), the camera follows once the player passes the screen's
 * left-hand dead zone, and the playfield streams new metatile columns
 * into whichever physical nametable is currently off-screen as the
 * camera advances past the initial two-nametable (512px) window --
 * that reuse of physical VRAM for logically further-along level content
 * is the "wraparound" acceptance criterion. A one-pixel... one-TILE
 * (8px) movement granularity is a deliberate simplification (see
 * FORMAT.md) that lets the mid-frame HUD/playfield split below use the
 * textbook $2000+$2005 technique with a constant fine-X of zero.
 *
 * HUD/playfield split: sprite 0 (OAM slot 0, a fully-opaque sentinel
 * tile placed at the last HUD scanline) genuinely collides with HUD row
 * 1 every frame -- this is NROM (mapper 0 has no IRQ source, so an
 * MMC3-style IRQ split is not an option here) and the split still uses
 * Super Mario Bros.'s status-bar mechanism in the sense that sprite 0's
 * hit is real and present every frame, BUT the split's own TIMING is
 * driven by a calibrated fixed delay, not a PPUSTATUS bit-6 poll -- see
 * main_loop()'s comment for the measurements that led to that deviation
 * and FORMAT.md's "HUD/playfield split" section for the acceptance-
 * criterion consequence. PPUCTRL's nametable-select bit and PPUSCROLL's
 * X are rewritten mid-frame; the automatic "hori(v)=hori(t)" copy every
 * PPU does at dot 257 of each scanline applies the new horizontal
 * scroll starting the very next scanline, without perturbing the
 * vertical scroll bits at all -- see main_loop()'s comment for why this
 * project deliberately does NOT use the $2006-only "commit v directly"
 * technique here (it would require reconstructing coarse/fine Y
 * mid-scanline, which $2000+$2005 never has to touch).
 */

#include "leveldata.h"

/* ---- PPU/APU/controller registers (raw memory-mapped, matching the
 * conductor-verified probe's style -- see ../../../../MASTER_PROMPT.md
 * ticket notes' probe main.c). Not cc65's <nes.h> PPU struct: this file
 * needs bit-level control over exactly when each register write happens
 * (the mid-frame split's whole point), which is clearer as named
 * addresses than through a struct. */
#define PPU_CTRL (*(volatile unsigned char *)0x2000)
#define PPU_MASK (*(volatile unsigned char *)0x2001)
#define PPU_STATUS (*(volatile unsigned char *)0x2002)
#define PPU_SCROLL (*(volatile unsigned char *)0x2005)
#define PPU_ADDR (*(volatile unsigned char *)0x2006)
#define PPU_DATA (*(volatile unsigned char *)0x2007)
#define OAM_DMA (*(volatile unsigned char *)0x4014)
#define JOYPAD1 (*(volatile unsigned char *)0x4016)

/* PPUCTRL, held at this value except bit0 (nametable-select), which
 * flips 0/1 for the mid-frame split:
 *   bit2=1  VRAM address +32 per $2007 write (column-major -- matches
 *           the level format's column_rle order, FORMAT.md)
 *   bit3=0  sprites read from CHR $0000 (SAME table as background --
 *           chr.s's module doc explains why: cc65's nes crt0
 *           unconditionally pulls in a 4096-byte built-in font that
 *           claims the other pattern table)
 *   bit4=0  background reads from CHR $0000
 *   bit7=0  NMI OFF for the whole program -- frame pacing is entirely
 *           by polling PPUSTATUS bit 7, never by vector */
#define PPUCTRL_BASE 0x04u
/* PPUMASK: show background + sprites, including their leftmost 8
 * pixels (bits 1,2) -- avoids any left-edge clipping interaction with
 * sprite-0 hit detection (sprite_hit_tests' own "05.left_clip" ROM
 * exists because this interaction is a real hardware hazard). */
#define PPUMASK_ON 0x1Eu

/* OAM shadow buffer. $0200-$04FF is NOT covered by any MEMORY region in
 * cc65's nes.cfg (SRAM starts at $0500; see that file's own comment
 * "$0200-$0500 3 pages for ppu memory write buffer" -- a convention for
 * cc65's higher-level screen helpers, which this program never calls),
 * so the linker never places any C symbol there; a raw pointer at
 * $0200 is exactly as safe here as the PPU register defines above, and
 * conveniently already page-aligned for OAM DMA. */
#define OAM_SHADOW ((volatile unsigned char *)0x0200)

/* OAM slot 0 MUST be the sprite-0 sentinel -- sprite-0 hit is defined in
 * terms of OAM index 0 specifically, not "any opaque sprite". */
#define OAM_SPRITE0_SLOT 0
#define OAM_PLAYER_SLOT 1

#define SPRITE0_Y 14u  /* renders scanline 15 -- last HUD scanline */
#define SPRITE0_X 8u   /* clear of the 0-7 left-clip region */
#define PLAYER_Y 215u  /* renders scanlines 216-223, feet at 224 (ground) */

/* Calibrated bridge to sprite 0's fixed scanline (main_loop()'s doc):
 * measured against crates/rf-harness/tests/rf_scroller_explore.rs's
 * reported scanlines, iterated to the smallest value that lands the
 * split reliably inside vblank or the split-good window (14-18). Not
 * derived analytically -- cc65's unoptimized codegen cost per source
 * line is not predictable enough for that; see FORMAT.md. */
#define SPLIT_DELAY 12u

#define SCREEN_W_PX 256u
#define SCREEN_W_TILES 32u
#define LEVEL_W_METATILES SCREEN_COLS /* 48 */
#define MAX_CAMERA_X 512u             /* 2 nametables' worth, pixels */
#define CAMERA_DEADZONE 112u          /* player screen-X before camera follows */
#define MAX_PLAYER_X 752u             /* level end -- see FORMAT.md */
#define STREAM_MARGIN_TILES 2u

/* ---- Documented state addresses (FORMAT.md / the ticket report record
 * the addresses the linker actually assigned these at -- see the build
 * log / .map file referenced there; declared in this fixed order so a
 * rebuild from unchanged source assigns the same addresses again). ----
 */
unsigned int player_x;         /* world X, pixels, 0..MAX_PLAYER_X */
unsigned int camera_x;         /* screen-left world X, pixels, 0..512 */
unsigned char columns_streamed; /* highest RAW tile-column index (0..95)
                                    known-correct in VRAM right now --
                                    the wraparound witness: this can only
                                    exceed 63 (2 nametables' worth) once
                                    a physical nametable has genuinely
                                    been reused for new content. */
static unsigned char frame_counter;
static unsigned char metatile_scratch[SCREEN_ROWS]; /* decode_column()'s
                                                        output: one
                                                        metatile ID per
                                                        metatile row. */

/* raw-tile-column blit staging, used only by init_video()'s preload loop
 * (rendering is off there -- forced blank -- so there is no cycle budget
 * to respect; prepare_raw_column()/blit_pending_column() below decode
 * and blit an entire 28-row column as one lump, which is fine only in
 * that context). main_loop()'s *runtime* streaming path does NOT use
 * these two -- see stream_chunk()'s doc for why a whole-column lump is
 * unaffordable once rendering is on. */
static unsigned char pending_tiles[28]; /* raw tile IDs, rows 2..29 */
static unsigned char pending_raw_col;
static unsigned char pending_valid;

/* ---- Incremental runtime streaming state (module doc below, at
 * stream_chunk()) -- these fields are the resumable cursor into "decode
 * + blit raw column `stream_col`", persisted across frames precisely so
 * that no single frame has to pay for the whole column at once. */
#define STREAM_CHUNK_ROWS 2u
static unsigned char stream_busy;         /* 1 while a column is in flight */
static unsigned char stream_col;          /* raw column index, valid when busy */
static unsigned char stream_row;          /* next metatile row to emit, 0..14 */
static unsigned char stream_rle_offset;   /* cursor into level_rle_data */
static unsigned char stream_run_remaining; /* rows left in the current RLE run */
static unsigned char stream_run_id;        /* metatile id of the current run */

/* Decode one metatile-column's column-RLE data (FORMAT.md "Column-RLE
 * encoding") into `out[SCREEN_ROWS]`, one metatile ID per row. Runs
 * until SCREEN_ROWS rows have been produced; a level file whose column
 * data doesn't sum to exactly SCREEN_ROWS would silently over/under-run
 * here in a shipped decoder, which is exactly why level_data.c's own
 * comment records that every column was verified (offline) to sum to
 * SCREEN_ROWS before being pasted in -- this runtime reader trusts that,
 * matching what a from-scratch reader of a level format still under
 * design (W5-02a is the real decoder) reasonably does. */
static void decode_column(unsigned char col, unsigned char *out) {
    unsigned char offset = level_column_offset[col];
    unsigned char row = 0;
    while (row < SCREEN_ROWS) {
        unsigned char run = level_rle_data[offset];
        unsigned char id = level_rle_data[offset + 1];
        unsigned char i;
        offset += 2;
        for (i = 0; i < run; i++) {
            out[row] = id;
            row++;
        }
    }
}

/* Decode raw tile-column `raw_col` (0..95) into `pending_tiles[28]` (one
 * byte per raw tile row, 2..29) and record it as the next column to
 * blit. Touches no PPU register -- safe to call from the "pure game
 * logic" phase (any time), which is the whole point (module doc). Used
 * by init_video()'s preload loop only -- main_loop()'s own runtime
 * streaming path uses stream_chunk() instead (its own doc explains
 * why a whole-column-per-call design like this one doesn't fit once
 * rendering is on). */
static void prepare_raw_column(unsigned char raw_col) {
    unsigned char mc = raw_col >> 1;      /* which metatile column */
    unsigned char side = raw_col & 0x01u; /* 0 = left tile, 1 = right */
    unsigned char metatile_row;
    unsigned char out_idx = 0;

    decode_column(mc, metatile_scratch);
    for (metatile_row = 0; metatile_row < SCREEN_ROWS; metatile_row++) {
        const unsigned char *row = metatile_table[metatile_scratch[metatile_row]];
        pending_tiles[out_idx] = row[side];          /* top */
        pending_tiles[out_idx + 1] = row[2u + side]; /* bottom */
        out_idx += 2;
    }
    pending_raw_col = raw_col;
    pending_valid = 1;
}

/* Blit `pending_tiles` (already-decoded by prepare_raw_column()) into
 * whichever physical nametable currently owns `pending_raw_col`
 * (FORMAT.md's "Streaming writes" section): one $2006 address setup,
 * then 28 plain `PPU_DATA = pending_tiles[i]` writes relying on
 * PPUCTRL's +32 auto-increment (PPUCTRL_BASE, set once by init_video()
 * and never changed except for the mid-frame split's nametable-select
 * bit) to walk straight down the column. Must only be called while
 * safely inside vblank. Used by init_video()'s preload loop only -- see
 * prepare_raw_column()'s doc. */
static void blit_pending_column(void) {
    unsigned char physical_col = pending_raw_col & 0x3Fu; /* mod 64 */
    unsigned char nametable = (physical_col < 32u) ? 0u : 1u;
    unsigned char tile_x = physical_col & 0x1Fu; /* mod 32 within the NT */
    unsigned int addr = 0x2000u + 64u /* 2 HUD rows */ + tile_x;
    unsigned char i;
    if (nametable) {
        addr += 0x0400u;
    }

    PPU_ADDR = (unsigned char)(addr >> 8);
    PPU_ADDR = (unsigned char)(addr & 0xFFu);
    for (i = 0; i < 28; i++) {
        PPU_DATA = pending_tiles[i];
    }
    pending_valid = 0;
}

/* Incremental runtime column streamer -- the fix for the bug this
 * ticket's report records at length (empirically root-caused, not
 * guessed): decoding an ENTIRE 14-row metatile column via decode_column()
 * plus expanding it via metatile_table plus blitting all 28 tiles, ALL
 * inside one frame's vblank-safe window, overran that window by roughly
 * 3-8x under cc65's own (unoptimized, and even under -O still
 * insufficient) codegen -- measured directly via per-write PPU cycle
 * timestamps from rf-nes's CoreEvent::ScrollWrite stream, not inferred.
 * No single piece of that work (decode_column alone, the metatile_table
 * expansion alone, or the 28-write blit alone) was cheap enough on its
 * own to explain the overrun away by optimizing just one of them; all
 * three needed to shrink, and even then the margin was too tight to
 * trust. The fix used by real NES games for exactly this situation is to
 * spread VRAM writes across many frames instead of doing them as one
 * lump -- this function processes STREAM_CHUNK_ROWS (2) metatile rows
 * per call (one call per frame, from main_loop()'s vblank-safe section),
 * decoding the column's RLE data incrementally (resuming the run cursor
 * across calls) and writing each chunk's tiles directly via $2007 as
 * they're produced (no pending_tiles staging buffer needed at all: the
 * PPU's own +32 auto-increment means a chunk's PPU_ADDR only has to be
 * set once, at the chunk's starting row).
 *
 * 14 rows / 2 rows-per-chunk = 7 frames to fully stream one raw column.
 * Movement advances one raw column's worth of camera position every 8
 * frames when the player holds Right (main_loop()'s frame_counter&7
 * gate, itself chosen for this reason -- see that comment), giving a
 * demand rate of 1.75 rows/frame against this function's 2 rows/frame
 * supply, so steady-state streaming does not fall behind. This does NOT
 * fully solve the timing problem it exists to fix, though: a 2-row
 * chunk is cheap on its own, but on frames where it actually runs it
 * still measurably pushes the mid-frame split several scanlines past
 * the split-good window (14-18) -- see the ticket report's
 * mutation/verification section for the measured scanlines and for why
 * this is shipped as a documented, characterized residual defect rather
 * than chased further. */
static void stream_chunk(unsigned char needed_raw_col) {
    unsigned char side;
    unsigned char physical_col;
    unsigned char nametable;
    unsigned char tile_x;
    unsigned int addr;
    unsigned char rows_this_chunk = 0;

    if (!stream_busy) {
        if (columns_streamed >= needed_raw_col || columns_streamed >= 95u) {
            return; /* nothing to do this frame */
        }
        stream_col = (unsigned char)(columns_streamed + 1);
        stream_row = 0;
        stream_rle_offset = level_column_offset[stream_col >> 1];
        stream_run_remaining = 0;
        stream_busy = 1;
    }

    side = stream_col & 0x01u;
    physical_col = stream_col & 0x3Fu;
    nametable = (physical_col < 32u) ? 0u : 1u;
    tile_x = physical_col & 0x1Fu;
    addr = 0x2000u + 64u /* 2 HUD rows */ + tile_x + 32u * (unsigned int)(stream_row * 2u);
    if (nametable) {
        addr += 0x0400u;
    }
    PPU_ADDR = (unsigned char)(addr >> 8);
    PPU_ADDR = (unsigned char)(addr & 0xFFu);

    while (rows_this_chunk < STREAM_CHUNK_ROWS && stream_row < SCREEN_ROWS) {
        const unsigned char *mt;
        if (stream_run_remaining == 0) {
            stream_run_remaining = level_rle_data[stream_rle_offset];
            stream_run_id = level_rle_data[stream_rle_offset + 1u];
            stream_rle_offset += 2u;
        }
        mt = metatile_table[stream_run_id];
        PPU_DATA = mt[side];        /* top tile of this metatile row */
        PPU_DATA = mt[2u + side];   /* bottom tile of this metatile row */
        stream_run_remaining--;
        stream_row++;
        rows_this_chunk++;
    }

    if (stream_row >= SCREEN_ROWS) {
        columns_streamed = stream_col;
        stream_busy = 0;
    }
}

/* Full controller-1 8-bit read (A,B,Select,Start,Up,Down,Left,Right);
 * only Right is used by this fixture (FORMAT.md), but the strobe+8-read
 * protocol is implemented in full since a partial read is not a
 * documented hardware shortcut. */
static unsigned char read_right(void) {
    unsigned char i;
    unsigned char last = 0;
    JOYPAD1 = 1;
    JOYPAD1 = 0;
    for (i = 0; i < 8; i++) {
        last = JOYPAD1 & 0x01u;
    }
    return last; /* 8th read = Right */
}

/* One-time forced-blank setup: palettes, both nametables' HUD rows +
 * attribute tables, and the initial two-nametable (512px) metatile
 * preload (FORMAT.md: "streamed" and "preloaded" are the same code
 * path, stream_metatile_column(), called here for mc 0..31 and again
 * from main_loop() for mc 32+). PPUMASK stays 0 (rendering off)
 * throughout this function. */
static void init_video(void) {
    unsigned char i;
    unsigned char raw_col;

    PPU_MASK = 0x00;
    (void)PPU_STATUS; /* reset the $2005/$2006 write-toggle latch */

    /* PPU warm-up: two vblanks before touching palette/VRAM, matching
     * the conductor-verified probe. */
    for (i = 0; i < 2; i++) {
        while ((PPU_STATUS & 0x80u) == 0) {
        }
    }

    /* +1 increment mode for this block's sequential/horizontal writes
     * (palettes, HUD rows, attribute tables) -- restored to +32
     * (PPUCTRL_BASE) below before any column-major nametable write. */
    PPU_CTRL = 0x00;

    PPU_ADDR = 0x3F;
    PPU_ADDR = 0x00;
    for (i = 0; i < 4; i++) {
        PPU_DATA = area_palette[i];
    }
    PPU_ADDR = 0x3F;
    PPU_ADDR = 0x10;
    for (i = 0; i < 4; i++) {
        PPU_DATA = sprite_palette[i];
    }

    /* HUD rows, NT0 only (the split always shows NT0's rows 0-1 -- see
     * main_loop()). Row 0 = tile $0C, row 1 = tile $0B (fully opaque,
     * sprite-0's BG collision partner). */
    PPU_ADDR = 0x20;
    PPU_ADDR = 0x00;
    for (i = 0; i < SCREEN_W_TILES; i++) {
        PPU_DATA = 0x0C;
    }
    PPU_ADDR = 0x20;
    PPU_ADDR = 0x20;
    for (i = 0; i < SCREEN_W_TILES; i++) {
        PPU_DATA = 0x0B;
    }

    /* Attribute tables, both nametables, all zero -- single global
     * background palette (FORMAT.md), so no attribute streaming is ever
     * needed at runtime. */
    PPU_ADDR = 0x23;
    PPU_ADDR = 0xC0;
    for (i = 0; i < 64; i++) {
        PPU_DATA = 0x00;
    }
    PPU_ADDR = 0x27;
    PPU_ADDR = 0xC0;
    for (i = 0; i < 64; i++) {
        PPU_DATA = 0x00;
    }

    /* Back to +32 (column-major) for every remaining $2007 use, for the
     * rest of the program's lifetime. */
    PPU_CTRL = PPUCTRL_BASE;

    /* No vblank-budget constraint here (rendering is off, forced
     * blank) -- prepare+blit back to back for all 64 raw columns of
     * the initial two-nametable window. */
    for (raw_col = 0; raw_col < 64; raw_col++) {
        prepare_raw_column(raw_col);
        blit_pending_column();
    }
    columns_streamed = 63;

    /* Hide all 64 OAM sprites, then place the two this game uses.
     * `i` counts SPRITES (0..63), not byte offsets, specifically so it
     * never has to compare an 8-bit counter against 256 -- an unsigned
     * char loop bound of 256 can never be reached (max 255) and
     * `i += 4` from 252 wraps to 0, which would silently never
     * terminate. */
    for (i = 0; i < 64; i++) {
        OAM_SHADOW[i * 4] = 0xFF;
    }
    OAM_SHADOW[OAM_SPRITE0_SLOT * 4 + 0] = SPRITE0_Y;
    OAM_SHADOW[OAM_SPRITE0_SLOT * 4 + 1] = 0x0E; /* sprite CHR tile $0E */
    OAM_SHADOW[OAM_SPRITE0_SLOT * 4 + 2] = 0x00;
    OAM_SHADOW[OAM_SPRITE0_SLOT * 4 + 3] = SPRITE0_X;

    player_x = 16;
    camera_x = 0;
    frame_counter = 0;
    OAM_SHADOW[OAM_PLAYER_SLOT * 4 + 0] = PLAYER_Y;
    OAM_SHADOW[OAM_PLAYER_SLOT * 4 + 1] = 0x0D; /* sprite CHR tile $0D */
    OAM_SHADOW[OAM_PLAYER_SLOT * 4 + 2] = 0x00;
    OAM_SHADOW[OAM_PLAYER_SLOT * 4 + 3] = (unsigned char)(player_x - camera_x);
}

/* The main superloop. Runs forever; every iteration is exactly one NES
 * frame, paced entirely by polling PPUSTATUS (module doc). */
static void main_loop(void) {
    for (;;) {
        unsigned char right_held;
        unsigned int needed_raw_col;
        unsigned int delay;
        unsigned char nt_x;

        /* ---- wait for vblank ---- */
        while ((PPU_STATUS & 0x80u) == 0) {
        }

        /* ---- pure game logic (safe anywhere -- no PPU access) ---- */
        right_held = read_right();
        frame_counter++;
        /* 8px every 8 frames (1px/frame average), not every 4: this rate
         * is a budget-derived constant, not a pacing choice -- see
         * stream_chunk()'s doc. One raw tile-column (8px) of camera
         * advance requires streaming 14 metatile rows; STREAM_CHUNK_ROWS
         * rows land per frame, so sustained throughput is
         * STREAM_CHUNK_ROWS rows/frame and demand is
         * 14/movement-period rows/frame. At a 4-frame period (demand 3.5
         * rows/frame) no STREAM_CHUNK_ROWS simultaneously fits the
         * ~2800-cycle vblank+split budget (measured: 3 rows/frame still
         * overshoots to scanline ~20-24, 4 rows/frame worsens it to
         * ~25-34) AND keeps up with demand (measured: 2 and 3 rows/frame
         * both fall behind at a 4-frame period). Halving the period to 8
         * frames (demand 1.75 rows/frame) lets STREAM_CHUNK_ROWS=2 clear
         * both constraints at once -- see the ticket report for the
         * measurements this constant is derived from. */
        if ((frame_counter & 0x07u) == 0 && right_held && player_x < MAX_PLAYER_X) {
            player_x += 8;
        }
        camera_x = (player_x > CAMERA_DEADZONE) ? (player_x - CAMERA_DEADZONE) : 0;
        if (camera_x > MAX_CAMERA_X) {
            camera_x = MAX_CAMERA_X;
        }

        /* Advance the incremental column streamer by one chunk (module
         * doc at stream_chunk()) if the camera's approach margin now
         * needs a column beyond what's fully streamed. Clamped at 95:
         * the level has no content past raw tile-column 95.
         *
         * This replaced an earlier single-frame "decode the whole
         * column, then blit all 28 tiles" design that measurably
         * overran the vblank+split cycle budget by 3-8x on every
         * streaming frame (root-caused via per-write PPU cycle
         * timestamps from the CoreEvent::ScrollWrite stream -- see the
         * ticket report's mutation/verification section for the actual
         * before/after measurements) rather than any single cause like
         * an unbounded poll or a function-call boundary -- both of
         * those were tried and neither fixed it, which is what led to
         * measuring the real cost directly instead of guessing again.
         * stream_chunk() is the fix: same total work, spread thin
         * enough per frame to fit. */
        needed_raw_col = (camera_x >> 3) + SCREEN_W_TILES + STREAM_MARGIN_TILES;
        if (needed_raw_col > 95u) {
            needed_raw_col = 95u;
        }
        stream_chunk((unsigned char)needed_raw_col);

        OAM_SHADOW[OAM_PLAYER_SLOT * 4 + 3] = (unsigned char)(player_x - camera_x);
        OAM_DMA = 0x02; /* copies $0200-$02FF into PPU OAM */

        /* Top-of-frame scroll: X=0, NT-select=0 (NT0) -- the HUD always
         * shows NT0's rows 0-1, whatever camera_x currently is. */
        PPU_CTRL = PPUCTRL_BASE; /* nt_x bit = 0 */
        PPU_SCROLL = 0;
        PPU_SCROLL = 0;

        PPU_MASK = PPUMASK_ON; /* idempotent after frame 0 */

        /* ---- end of vblank-only section ---- */

        /* Bridge to sprite-0's scanline with a CALIBRATED fixed delay,
         * then fire the split unconditionally -- not a poll. Sprite 0
         * never moves (SPRITE0_Y=14/SPRITE0_X=8, both compile-time
         * constants): it hits at the same scanline every single frame,
         * so "poll PPUSTATUS until the hardware tells us" was solving a
         * problem this fixture doesn't have, at the cost of a poll bound
         * that had to cover a 3000-4000+ cycle worst-case gap from a
         * busy-loop start. That was tried at several bounds/types (10,
         * 200, 255, both unsigned char and unsigned int) and each either
         * skipped the split on a large fraction of frames (guard
         * exhausted before the hit occurred) or, once large enough,
         * risked costing enough cycles on exhaustion to overrun a whole
         * real frame -- see the ticket report for the measurements
         * (crates/rf-harness/tests/rf_scroller_explore.rs's
         * split_frames/skip_frames counters, added specifically to catch
         * silently-skipped splits) and for why polling was abandoned
         * rather than further tuned.
         *
         * SPLIT_DELAY below is a measured constant (calibrated against
         * that same test's reported scanlines for the SPLIT write
         * specifically, iterated a few times), not derived from a
         * cycle-counting model of cc65's codegen -- see FORMAT.md's
         * "HUD/playfield split" section for the number and how it was
         * obtained, and for the deviation this records: the split is
         * timer-driven, not sprite-0-hit-driven, even though sprite 0
         * (OAM slot 0, the sentinel tile) still renders and still
         * genuinely collides with HUD row 1 every frame -- FORMAT.md
         * records that PPUSTATUS bit 6 does assert during the HUD rows
         * as an independent hardware fact, just not as this code's
         * trigger. */
        for (delay = 0; delay < SPLIT_DELAY; delay++) {
        }

        /* Mid-frame split: $2000 (nametable-select bit) + $2005 x2
         * (coarse/fine X, then Y=0). Deliberately NOT the $2006
         * "commit v directly" technique -- $2006's second write sets
         * ALL of v, including coarse/fine Y, which at this exact
         * scanline (15) are mid-count (coarse Y=1, fine Y=7, about
         * to roll to coarse Y=2 at the next dot-256 increment); a
         * $2006 write here would have to reconstruct those non-zero
         * values to avoid resetting the playfield to nametable row
         * 0 (the HUD would render again inside the playfield -- a
         * one-row error invisible to a coarse eyeball but real).
         * $2000+$2005 needs none of that: $2000 changes only t's
         * nametable-select bits, $2005 changes only t's X bits (and
         * this program's own `x` fine-scroll register, always 0
         * because camera_x is tile-aligned by construction), and
         * the PPU's own dot-257 hori(v)=hori(t) copy -- which runs
         * unconditionally, every scanline -- applies the new
         * horizontal position starting the very next scanline
         * without ever touching v's vertical bits. See FORMAT.md /
         * the ticket report for the consequence: this fires
         * CoreEvent::ScrollWrite from write_scroll ($2005, ticket
         * W4-00's unconditional emission site), not from write_addr
         * ($2006, ticket W4-03a's active-picture-gated site) -- the
         * W4-03a gate remains unobserved by this fixture. */
        nt_x = (unsigned char)((camera_x >> 8) & 0x01u);
        PPU_CTRL = PPUCTRL_BASE | nt_x;
        PPU_SCROLL = (unsigned char)(camera_x & 0xFFu);
        PPU_SCROLL = 0;
    }
}

int main(void) {
    init_video();
    main_loop();
    return 0;
}
