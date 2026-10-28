-- HD-2D render plugin. Marks the objects the recipe can place so the renderer
-- can stand them up with a class-specific shadow. Facts used (all optional;
-- missing facts draw nothing):
--   player_x, player_y, camera_x, camera_y   16-bit RAM rows (world pixels)
--   object tables named in TABLES, fields x_lo, x_hi, y, y_hi, state
-- Output: rf.gui.rect(x, y, w, h, class_code) in picture pixels.
local PLAYER, ENEMY, PROJECTILE, PICKUP, SCENERY = 1, 2, 3, 4, 5
local SCREEN_W, SCREEN_H = 256, 240
local TABLES = {
  { name = "enemies", class = ENEMY },
  { name = "actors", class = ENEMY },
  { name = "shots", class = PROJECTILE },
  { name = "items", class = PICKUP },
  { name = "props", class = SCENERY },
}
local MAX_SLOTS = 24

local function u16(label)
  local a = rf.profile.addr(label)
  if a == nil then return nil end
  return rf.mem.read_u16(a)
end

local function visible(x, y, w, h)
  return x + w > 0 and x < SCREEN_W and y + h > 0 and y < SCREEN_H
end

function on_frame(frame)
  local cam_x = u16("camera_x") or 0
  local cam_y = u16("camera_y") or 0

  local px, py = u16("player_x"), u16("player_y")
  if px ~= nil and py ~= nil then
    local x, y = px - cam_x, py - cam_y
    if visible(x, y, 16, 32) then
      rf.gui.rect(x, y, 16, 32, PLAYER)
    end
  end

  for _, t in ipairs(TABLES) do
    for slot = 0, MAX_SLOTS - 1 do
      local lo = rf.profile.addr(t.name .. ".x_lo." .. slot)
      local ya = rf.profile.addr(t.name .. ".y." .. slot)
      if lo == nil or ya == nil then break end
      local st = rf.profile.addr(t.name .. ".state." .. slot)
      if st == nil or rf.mem.read_u8(st) ~= 0 then
        local hi = rf.profile.addr(t.name .. ".x_hi." .. slot)
        local yh = rf.profile.addr(t.name .. ".y_hi." .. slot)
        local wx = rf.mem.read_u8(lo) + ((hi and rf.mem.read_u8(hi) or 0) * 256)
        local wy = rf.mem.read_u8(ya) + ((yh and rf.mem.read_u8(yh) or 0) * 256)
        local x, y = wx - cam_x, wy - cam_y
        if visible(x, y, 16, 16) then
          rf.gui.rect(x, y, 16, 16, t.class)
        end
      end
    end
  end
end
