-- Sample logic plugin: on every entry of the recipe function "nmi", set the
-- recipe RAM byte named "mod_flag" to 1 -- but only in Enhanced mode.
--
-- The `rf.mode()` test is the whole contract. Original mode must stay
-- byte-identical, so a logic plugin does nothing there; delete the test and
-- the lockstep gate refuses the plugin (see the W39-00 tests). The address is
-- not in this file: it comes from the recipe's RAM facts.
local FLAG = rf.profile.addr("mod_flag")

rf.on_enter("nmi", function()
  if FLAG ~= nil and rf.mode() == "enhanced" then
    rf.mem.write_u8(FLAG, 1)
  end
end)
