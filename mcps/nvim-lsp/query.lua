-- One code-intelligence question, asked and answered, then exit.
--
-- Run as: nvim --headless -u <nvimrc/init.lua> -l query.lua <json-args>
--
-- One-shot rather than a long-lived RPC server on purpose: talking to a running
-- Neovim needs pynvim or a msgpack implementation, and this harness should not
-- gain a dependency to ask a question. The cost is a language-server cold start
-- per query; the answer reports how long it took so that cost is visible rather
-- than mysterious.

local ok, args = pcall(vim.json.decode, _G.arg[1] or '{}')
if not ok then
  io.stdout:write(vim.json.encode({ error = 'bad arguments' }))
  return
end

local started = vim.uv.hrtime()
local function finish(payload)
  payload.secs = tonumber(string.format('%.1f', (vim.uv.hrtime() - started) / 1e9))
  io.stdout:write(vim.json.encode(payload))
  vim.cmd('qa!')
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

-- Settle: give the server a moment past attach for initial indexing. Polling
-- for "not busy" is not portable across servers, so this waits for the request
-- to stop coming back empty instead, up to the same deadline.
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

local responses
local settle = vim.uv.hrtime() + deadline * 1e6
repeat
  responses = request()
  local any = false
  for _, r in pairs(responses or {}) do
    if r.result and (type(r.result) ~= 'table' or next(r.result) ~= nil) then
      any = true
    end
  end
  if any then break end
  vim.wait(500)
until vim.uv.hrtime() > settle

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
})
