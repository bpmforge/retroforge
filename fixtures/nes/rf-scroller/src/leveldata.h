/* RetroForge -- RF-Scroller (ticket W2-10) -- level data declarations.
 * See level_data.c for the tables themselves and ../FORMAT.md for the
 * on-ROM shape this header's constants describe. */
#ifndef RF_SCROLLER_LEVELDATA_H
#define RF_SCROLLER_LEVELDATA_H

#define METATILE_COUNT 4
#define SCREEN_ROWS 14      /* metatile rows in the scrolling playfield */
#define SCREEN_COLS 48      /* metatile columns in the whole level */
#define LEVEL_RLE_BYTES 216 /* total bytes in level_rle_data */

extern const unsigned char metatile_table[METATILE_COUNT][4];
extern const unsigned char collision_table[METATILE_COUNT];
extern const unsigned char area_palette[4];
extern const unsigned char sprite_palette[4];
extern const unsigned char level_column_offset[SCREEN_COLS];
extern const unsigned char level_rle_data[LEVEL_RLE_BYTES];

#endif
