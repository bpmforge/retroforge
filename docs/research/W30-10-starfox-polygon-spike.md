# W30-10 spike — can Star Fox's projected geometry be found in GSU RAM by observation?

Date: 2026-10-15. Clean-room: observation of our own emulator running the
user's dump only — no Star Fox source, no leaked source, no disassembly or
RAM map. Requires W30-00 (Super FX output fix); before it, the GSU never
drew a polygon.

## Answer

**Yes for projected vertices; face lists not yet found.** Screen-space
vertex coordinates appear in GSU RAM as **6-byte records** whose first two
words are `x` and `y` (little-endian u16), clustered at GSU RAM
`0x0780-0x0A3F` (bank `$70`), rebuilt every render job.

## Method

1. Temporary instrumentation (not committed) recorded every GSU `PLOT`
   (R1 = x, R2 = y, COLR) and cut the stream at each `STOP` (one render
   job), snapshotting GSU RAM at that `STOP`.
2. Runs of plots with x+1 on the same y and colour became spans; spans on
   consecutive y with the same colour became one polygon. Each polygon's
   top and bottom spans gave four approximate vertex candidates (their end
   points), skipping edge-clipped ones (x < 2, y < 2, x > 253, y > 189)
   and x == y.
3. Each job's RAM was scanned for adjacent u16 words `(x, y)` equal to a
   candidate. A control run shifted every candidate's y by 37 to measure
   chance matches.

## Evidence (Star Fox (USA), no input)

| Window | Jobs | Candidates | Found as `(x,y)` words | Control (y+37) | Where |
|---|---|---|---|---|---|
| frames 850-1050 (intro) | 56 | 15632 | 3589 (23.0%) | 197 (1.3%) | 0x07C0-0x0A3F |
| frames 2600-2800 (attract play) | 98 | 2255 | 329 (14.6%) | 0 (0.0%) | 0x0880-0x09BF |

The gaps between distinct hit addresses are dominated by **6** (36 of 61
gaps in the intro window, 8 of 13 in play): a record of three words,
`x, y` and one more (depth or flags — not identified). Hit rates are well
below 100% because span end points only approximate vertices (an edge's
top/bottom span ends at the vertex only when the vertex is a span corner),
so 15-23% against a 0-1.3% control is the meaningful comparison.

## Not established

- **Face lists**: which records form each polygon. Not searched yet.
- The third word of a record, the table's start/length per job (a count or
  terminator), and whether all objects share one table.
- Other GSU titles (Stunt Race FX, Vortex) — not tried.

## Suggested next steps before any Phase B build (Brad's call)

1. Pin the table: per job, find the first/last record and a count word
   that matches the number of distinct vertices plotted.
2. Find face lists: search GSU RAM for lists of small integers or offsets
   that index the 6-byte table, grouped like the polygons drawn that job.
3. Only if both hold reliably: a renderer that re-rasterises those
   polygons at high resolution (presentation only), validated against our
   own plotted frames.
