# Regenerating RF-Rooms-Flat's bytes

```
rooms = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]
image = b"".join(bytes([t]) * 4 for t in rooms)   # 24 bytes
```

`image` is the NORMALIZED (header-stripped) content; `room_grid_data` is
at offset `0x0000`. There is no index table — see FORMAT.md.

Verifiable hashes of that image:

```
sha256  66160ffc3425e84c739b51bb45b4a82dd600a0523cd2663fb54414191a2f949e
md5     8d887cdd630c244de27c9400ef708cd6
crc32   5c1b197c
```

These are hashes of the LEVEL IMAGE, not of a ROM — there is no ROM here.
