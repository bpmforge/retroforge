-- Example overlay: draw the player's position, read from PROFILE-PUBLISHED
-- addresses (tickets W4-04 and W5-07; FR-REND-006, MVP.md §3's "Lua script
-- draws live player-position overlay using profile-published addresses").
--
-- Everything this script can reach is what its manifest asked for. `io`,
-- `os`, `require`, `dofile` and friends are ABSENT from the sandbox, not
-- refused at call time -- see crates/rf-plugin-sdk/src/sandbox.rs.
--
-- NOTE WHAT THIS FILE NO LONGER CONTAINS: an address literal. W4-04's
-- version hardcoded `PLAYER_X = 0x0300`, which was both wrong for this
-- game (RF-Scroller's player_x is at $6029) and wrong in principle -- a
-- script that hardcodes an address works for exactly one game and breaks
-- silently when that game's profile is re-derived. Asking the profile is
-- the entire reason a profile publishes a memory map.

-- Resolved once at load: the map does not change while a game is running.
local PLAYER_X = rf.profile.addr("player_x")
local CAMERA_X = rf.profile.addr("camera_x")

function on_frame(frame)
  -- A profile that does not label a player position simply gets no
  -- overlay. Drawing at a guessed address would put a marker somewhere
  -- meaningless and look like a bug in the game.
  if PLAYER_X == nil then
    return
  end

  -- read_u16, because the profile declares player_x as u16 and this level
  -- is 768 pixels wide -- a u8 read would wrap at 255 and the marker
  -- would teleport back to the left edge a third of the way in.
  local x = rf.mem.read_u16(PLAYER_X)

  -- WORLD space: rf.gui draws in the same space W5-03's scene layers use,
  -- so a world-space value from the profile goes straight through with no
  -- camera arithmetic. Subtract camera_x yourself if you want the marker
  -- pinned to the viewport instead.
  rf.gui.rect(x - 4, 96, 8, 8)

  if CAMERA_X ~= nil and frame % 600 == 0 then
    print(string.format("frame %d  player_x %d  camera_x %d",
                        frame, x, rf.mem.read_u16(CAMERA_X)))
  end
end
