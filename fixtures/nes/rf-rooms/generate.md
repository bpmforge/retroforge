# Regenerating RF-Rooms' bytes

The construction rule, so the fixture is reproducible rather than a blob
somebody has to trust. `FORMAT.md` explains the layout and why it exists.

```
rooms = [0x10, 0x00, 0x20]        # floor, open, wall
data  = b"".join(bytes([t]) * 16 for t in rooms)   # 48 bytes, room 0 first
index = bytes([2,1,1,2, 2,0,0,2, 2,2,2,2])         # 12 bytes, reading order
image = data + index                                # 60 bytes total
```

`image` is the NORMALIZED (header-stripped) content. `room_grid_data` is
at offset `0x0000`, `room_grid_index` at `0x0030`.

The decoder's tests build these same bytes inline rather than reading this
file, so the fixture and the tests cannot drift apart silently: if the
rule here changes, the tests still assert the old bytes and go red.
