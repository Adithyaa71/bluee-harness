-- The query hook, loaded on top of whatever config Neovim already has.
--
-- Used when the persistent Neovim runs Adithya's nyx-nvim config (via
-- NVIM_APPNAME) instead of the minimal one beside this file: his config brings
-- the plugins his tool layer needs, and this adds only the entry point the
-- harness calls. Nothing here configures a language server - his config does
-- that with mason and lspconfig.

-- ---------------------------------------------------------------------------
-- The query entry point, for the persistent-server mode.
--
-- Arguments go in via a file and the answer comes out via a file, rather than
-- being threaded through `--remote-expr` as a quoted string. Quoting JSON
-- through a shell, into Vimscript, into Lua, is three escaping layers and every
-- one of them is a bug waiting to happen; two temp files are not.
local here = debug.getinfo(1, 'S').source:sub(2):gsub('[^/\\]+$', '')
local run = dofile(here .. 'query.lua')

function _G.bluee_query_file(inpath, outpath)
  local ok, res = pcall(function()
    local raw = table.concat(vim.fn.readfile(inpath), '\n')
    return run(vim.json.decode(raw))
  end)
  if not ok then
    res = { error = tostring(res) }
  end
  vim.fn.writefile({ vim.json.encode(res or { error = 'no result' }) }, outpath)
  return 'ok'
end

-- Something to poll for: the pipe answering proves Neovim is up, and this
-- proves the config finished loading.
_G.BLUEE_READY = true
