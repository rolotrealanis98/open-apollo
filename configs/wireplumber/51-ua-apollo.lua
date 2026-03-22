-- WirePlumber 0.4.x rules for Universal Audio Apollo interfaces
-- Deploy to: /etc/wireplumber/main.lua.d/51-ua-apollo.lua
--
-- NOTE: The pro-audio profile must be set via wpctl after PipeWire starts,
-- because WirePlumber explicitly excludes pro-audio from auto-selection.
-- The apollo-init.sh script handles this.

-- Node properties for Apollo output/input nodes
table.insert(alsa_monitor.rules, {
  matches = {
    {
      { "node.name", "matches", "alsa_output.*" },
      { "alsa.driver_name", "equals", "ua_apollo" },
    },
    {
      { "node.name", "matches", "alsa_input.*" },
      { "alsa.driver_name", "equals", "ua_apollo" },
    },
  },
  apply_properties = {
    -- Driver uses copy callbacks (no mmap support)
    ["api.alsa.disable-mmap"]           = true,

    -- Keep device open to avoid repeated open/close cycles
    -- (Apollo firmware can be sensitive to transport resets)
    ["session.suspend-timeout-seconds"]  = 0,

    -- Force S32_LE format (native Apollo format)
    ["audio.format"]                     = "S32LE",

    -- Default to 48kHz
    ["audio.rate"]                       = 48000,

    -- Use software volume — don't touch the Apollo hardware monitor level.
    ["api.alsa.soft-mixer"]              = true,

    -- Node descriptions for Sound Settings UI
    ["node.nick"]                        = "Apollo",
  },
})
