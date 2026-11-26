-- The scratch MCP server the e2e tier's jinn.toml points at: one
-- echo tool, network-free. Served over stdio by `jinn luau serve`.
jinn.tool(
  {
    name = "echo",
    description = "Echoes its input back",
    input = { type = "object", properties = { text = { type = "string" } } },
  },
  function(args)
    return "echo: " .. tostring(args.text or "")
  end
)
