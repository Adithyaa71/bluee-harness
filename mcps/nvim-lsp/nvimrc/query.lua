-- One code-intelligence question, answered against the LIVE Neovim.
--
-- Loaded once by init.lua and called per query, rather than run as a one-shot
-- script. That is the whole point: a fresh Neovim per question throws away
-- rust-analyzer's index every time, and indexing a Rust workspace is minutes.
-- Keeping one server alive makes the first question slow and the rest fast.
--
-- Returns a table; the caller encodes it. Nothing here exits Neovim.

return function(args)
local started = vim.uv.hrtime()
local result = nil
local function finish(payload)
  payload.secs = tonumber(string.format('%.1f', (vim.uv.hrtime() - started) / 1e9))
  result = payload
  return payload
end

local path = args.path
if not path or path == '' then
  return finish({ error = 'a path is required' })
end
if vim.fn.filereadable(path) == 0 then
  return finish({ error = 'no such file: ' .. path })
end

-- Open it. The root markers in the config decide the project root from here.
vim.cmd('edit ' .. vim.fn.fnameescape(path))
local buf = vim.api.nvim_get_current_buf()
local ft = vim.bo[buf].filetype

-- Wait for a language server to attach and stop reporting progress. Attachment
-- alone is not enough: rust-analyzer attaches immediately and then indexes for
-- many seconds, and asking it during indexing returns an empty list, which
-- would read as "nothing calls this" and be worse than waiting.
local deadline = (args.timeout or 60) * 1000
local waited = vim.wait(deadline, function()
  return #vim.lsp.get_clients({ bufnr = buf }) > 0
end, 100)

if not waited then
  return finish({
    error = ('no language server attached to a %s file within %ds'):format(
      ft == '' and 'unknown-type' or ft, (args.timeout or 60)),
    filetype = ft,
    servers_available = _G.BLUEE_LSP_ENABLED or {},
    hint = 'Install a language server for this filetype and it will be picked up '
        .. 'automatically - the config registers only the ones present on PATH.',
  })
end

-- Wait for indexing to finish before asking.
--
-- Attachment is not readiness: rust-analyzer attaches in a second and then
-- indexes the dependency graph for minutes on a cold cache, answering every
-- request with an empty list meanwhile. An empty list reads as "nothing calls
-- this", which is worse than waiting - so wait on the server's own progress
-- reports instead of guessing. vim.lsp.status() is '' exactly when no client
-- has work outstanding.
local uses_ra = false
for _, c in ipairs(vim.lsp.get_clients({ bufnr = buf })) do
  if c.name == 'rust_analyzer' then uses_ra = true end
end

local idle_since = nil
vim.wait(deadline, function()
  -- rust-analyzer says so itself; everything else is judged by its progress
  -- reports going quiet for a stretch.
  if uses_ra then return _G.BLUEE_RA_READY == true end
  if vim.lsp.status() ~= '' then
    idle_since = nil
    return false
  end
  -- Progress reports arrive in bursts with gaps between them, and the first
  -- gap is not the end.
  idle_since = idle_since or vim.uv.hrtime()
  return (vim.uv.hrtime() - idle_since) > 1.5e9
end, 200)

local ready = uses_ra and (_G.BLUEE_RA_READY == true) or (vim.lsp.status() == '')

local method = args.method or 'textDocument/references'
local line = (args.line or 1) - 1
local col = args.col or 0

local function request()
  local params = {
    textDocument = vim.lsp.util.make_text_document_params(buf),
    position = { line = line, character = col },
  }
  if method == 'textDocument/references' then
    params.context = { includeDeclaration = args.include_declaration or false }
  end
  if method == 'textDocument/documentSymbol' then
    params = { textDocument = vim.lsp.util.make_text_document_params(buf) }
  end
  return vim.lsp.buf_request_sync(buf, method, params, 20000)
end

-- One retry after a pause, in case the server was mid-flight when we asked.
-- Not a loop until non-empty: that turns "genuinely zero references" into a
-- full-timeout wait and then reports the same empty answer anyway.
local responses = request()
local function empty(rs)
  for _, r in pairs(rs or {}) do
    if r.result and (type(r.result) ~= 'table' or next(r.result) ~= nil) then
      return false
    end
  end
  return true
end
if empty(responses) then
  vim.wait(2000)
  responses = request()
end
local still_busy = not ready

-- Shape the answer. Locations become file:line plus the source line itself,
-- because a bare file:line tells a model nothing and it would have to read
-- every file to find out what is there.
local function line_text(file, n)
  local lines = vim.fn.readfile(file, '', n)
  local t = lines[n]
  if not t then return '' end
  return vim.trim(t):sub(1, 200)
end

local out = {}
for _, resp in pairs(responses or {}) do
  local r = resp.result
  if r then
    if method == 'textDocument/hover' then
      local c = r.contents
      local text = ''
      if type(c) == 'string' then text = c
      elseif c and c.value then text = c.value
      elseif c and c[1] then text = type(c[1]) == 'string' and c[1] or (c[1].value or '') end
      return finish({ hover = text, filetype = ft })
    elseif method == 'textDocument/documentSymbol' then
      local function walk(items, depth)
        for _, sym in ipairs(items or {}) do
          local range = sym.range or (sym.location and sym.location.range)
          table.insert(out, {
            name = sym.name,
            kind = vim.lsp.protocol.SymbolKind[sym.kind] or tostring(sym.kind),
            line = range and (range.start.line + 1) or 0,
            depth = depth,
          })
          if sym.children then walk(sym.children, depth + 1) end
        end
      end
      walk(r, 0)
    else
      if r.uri or r.targetUri then r = { r } end
      for _, loc in ipairs(r) do
        local uri = loc.uri or loc.targetUri
        local range = loc.range or loc.targetSelectionRange
        if uri and range then
          local file = vim.uri_to_fname(uri)
          table.insert(out, {
            file = file,
            line = range.start.line + 1,
            col = range.start.character + 1,
            text = line_text(file, range.start.line + 1),
          })
        end
      end
    end
  end
end

finish({
  count = #out,
  results = out,
  filetype = ft,
  method = method,
  -- Told plainly rather than left to look like a real zero: an empty answer
  -- from a server that is still indexing means "ask again", not "no callers".
  indexing = still_busy or nil,
  note = (#out == 0 and still_busy)
      and 'The language server was still indexing, so this is not a reliable '
       .. 'zero - ask again in a minute.' or nil,
})

return result
end
