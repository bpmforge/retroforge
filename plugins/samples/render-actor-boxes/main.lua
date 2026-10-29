-- Sample render plugin: a box on every active slot of the recipe's "actors"
-- object table. Labels come from the recipe: "<table>.<field>.<slot>".
function on_frame(frame)
  -- 16 slots at most; stops at the first slot the recipe does not describe.
  for slot = 0, 15 do
    local state = rf.profile.addr("actors.state." .. slot)
    local xa = rf.profile.addr("actors.x_lo." .. slot)
    local ya = rf.profile.addr("actors.y." .. slot)
    if state == nil or xa == nil or ya == nil then
      break
    end
    if rf.mem.read_u8(state) ~= 0 then
      rf.gui.rect(rf.mem.read_u8(xa) - 4, rf.mem.read_u8(ya) - 4, 8, 8)
    end
  end
end
