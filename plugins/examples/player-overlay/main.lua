-- Example overlay: draw the player's position, read from profile-published
-- addresses (ticket W4-04; FR-REND-006, MVP.md §3's "Lua script draws live
-- player-position overlay using profile-published addresses").
--
-- Everything this script can reach is what its manifest asked for. `io`,
-- `os`, `require`, `dofile` and friends are ABSENT from the sandbox, not
-- refused at call time -- see crates/rf-plugin-sdk/src/sandbox.rs.

-- RF-Scroller's player X, per profiles/nes/rf-scroller-demo/profile.toml.
local PLAYER_X = 0x0300

function on_frame(frame)
  local x = rf.mem.read_u8(PLAYER_X)

  -- A small box that tracks the player horizontally.
  rf.gui.rect(x, 200, 8, 8)
  rf.gui.text(4, 4, string.format("frame %d  player_x %d", frame, x))

  -- print() goes to the Lua console panel, not the host's stdout.
  if frame % 600 == 0 then
    print(string.format("still running at frame %d", frame))
  end
end
