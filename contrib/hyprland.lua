-- drove: agent herding for Hyprland (Lua config).
-- Put this in ~/.config/hypr/drove.lua and load it from hyprland.lua with
--   require("drove")        (or dofile(os.getenv("HOME") .. "/.config/hypr/drove.lua"))
-- Adjust `drove` if the binary is not on Hyprland's PATH.

local drove = "drove"
local mainMod = "SUPER"

-- Start the daemon with the session. Skip this if you use contrib/drove.service.
hl.on("hyprland.start", function()
    hl.exec_cmd(drove .. " daemon")
end)

-- Pick an agent (fuzzel/wofi/rofi, see [picker] in ~/.config/drove/config.toml).
hl.bind(mainMod .. " + A", hl.dsp.exec_cmd(drove .. " pick"))
-- Jump to the agent that needs you most (needs input > finished turn > anything flagged).
hl.bind(mainMod .. " + N", hl.dsp.exec_cmd(drove .. " next"))
-- New Claude Code agent in $HOME (use a profile name from config.toml for others).
hl.bind(mainMod .. " + SHIFT + A", hl.dsp.exec_cmd(drove .. " spawn claude --cwd ~"))
-- Show/hide agents when they live on a special workspace ([spawn] workspace = "special:agents").
hl.bind(mainMod .. " + grave", hl.dsp.workspace.toggle_special("agents"))

-- Optional: rules for every agent window (class is drove-<id>).
-- hl.window_rule({
--     name  = "drove-agents",
--     match = { class = "^drove-.*$" },
--     float = true,
--     size  = "1200 800",
-- })
