-- WirePlumber 0.4.x rules for Universal Audio Apollo interfaces
-- Deploy to: /etc/wireplumber/main.lua.d/51-ua-apollo.lua

-- Force pro-audio profile on the Apollo ALSA device.
-- Without this, WirePlumber picks the default profile which has no sinks.
table.insert(alsa_monitor.rules, {
  matches = {
    {
      { "device.name", "matches", "alsa_card.*" },
      { "alsa.driver_name", "equals", "ua_apollo" },
    },
  },
  apply_properties = {
    -- Select the pro-audio profile automatically
    ["device.profile"] = "pro-audio",
  },
})

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

    -- Force quantum 512 (~10.7ms) when Apollo is the graph driver.
    -- 256 crashes during PipeWire restart races. 512 is stable + no drift.
    ["node.force-quantum"]               = 512,

    -- Force S32_LE format (native Apollo format)
    ["audio.format"]                     = "S32LE",

    -- Default to 48kHz
    ["audio.rate"]                       = 48000,

    -- Use software volume — don't touch the Apollo hardware monitor level.
    ["api.alsa.soft-mixer"]              = true,

    -- Shared clock name — loopback nodes use the same clock.name
    -- so PipeWire skips adaptive resampling between them
    ["clock.name"]                       = "ua_apollo",

    -- Lower DLL bandwidth for smoother clock lock-in.
    -- Default (~100) causes pitch wobble during initial sync.
    ["api.alsa.dll-bandwidth-max"]       = 12,


    -- Node descriptions for Sound Settings UI
    ["node.nick"]                        = "Apollo",

    -- Prefer Apollo over other audio devices (HDMI, built-in)
    ["priority.driver"]                  = 2000,
    ["priority.session"]                 = 2000,
  },
})
