# W39-02 screenshots: object-aware Diorama

Made by `RF_W39_SHEET_DIR=docs/screens/w39 cargo test -p rf-renderer --test
w39_02_object_classes contact_sheet`. Synthetic scene, no ROM content: flat
coloured sprites on a checker floor, morning sun.

- `contact-sheet.png` - left: no recipe (every sprite gets the W34 shadow, standing on the floor); right: classes from the recipe, left to right player (blue: nearer, firmer shadow), enemy (red: unchanged), projectile (yellow: floats, small faint shadow), pickup (green: hovers, smaller shadow), scenery (brown: no shadow).

# W39-04 screenshots: HD-2D render plugin

Made by `RF_W39_SHEET_DIR=docs/screens/w39 cargo test -p rf-renderer --test
w39_04_hd2d screenshots`. A made-up level (no ROM content): sky, hills, brick
floor and platforms, five blobs standing for the five object classes (player,
enemy, projectile, pickup, scenery). The mirrored repeats at the left and
right edges are the Diorama's overscan (W35-01).

- `hd2d-synthetic.png` - walls extruded from the depth grid, tilted camera, sun and shadows, depth of field centred on the player's feet.
- `hd2d-synthetic-no-dof.png` - the same without the depth of field.
- `hd2d-synthetic-dusk.png` - low evening sun, camera swung the other way.
- `hd2d-synthetic-flat.png` - no walls, near straight-on: what the extrusion and tilt add.

SMW demo frames are written outside the repo by the ROM-gated
`crates/retroforge/tests/w39_04_hd2d_plugin.rs` (law 5: ROM-derived images are not committed).
