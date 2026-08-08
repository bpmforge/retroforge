/* RetroForge -- RF-Scroller (ticket W2-10) -- level data declarations.
 * See level_data.c for the tables themselves and ../FORMAT.md for the
 * on-ROM shape this header's constants describe. */
#ifndef RF_SCROLLER_LEVELDATA_H
#define RF_SCROLLER_LEVELDATA_H

#define METATILE_COUNT 4
#define SCREEN_ROWS 14      /* metatile rows in the scrolling playfield */
#define SCREEN_COLS 48      /* metatile columns in the whole level */
#define LEVEL_RLE_BYTES 246 /* total bytes in level_rle_data -- grew from
                                216 at ticket W2-10a, which replaced
                                metatile columns 44-47's content with the
                                vertical sub-area's two-landmark-band
                                "ladder" pattern (FORMAT.md); still <=255
                                so level_column_offset's `unsigned char`
                                entries (and main.c's decode_column()/
                                stream_chunk()'s `unsigned char` RLE-data
                                cursors) never need to represent an offset
                                that doesn't fit in one byte -- verified by
                                the generation script, see level_data.c's
                                own comment. */

extern const unsigned char metatile_table[METATILE_COUNT][4];
extern const unsigned char collision_table[METATILE_COUNT];
extern const unsigned char area_palette[4];
extern const unsigned char sprite_palette[4];
extern const unsigned char level_column_offset[SCREEN_COLS];
extern const unsigned char level_rle_data[LEVEL_RLE_BYTES];

#endif
