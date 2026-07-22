-- Project-local Neovim config for firepath, loaded via `exrc`
--
-- It starts the language server from the binary built from this repo
local root = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local bin = root .. "/target/debug/firepath"

local group = vim.api.nvim_create_augroup("firepath", { clear = true })

-- The server's semantic token types are custom as no standard LSP type names a
-- ledger date, payee, account, amount, or commodity
local token_highlights = {
	["@lsp.type.date"] = "@string.special",
	-- tree-sitter has no payee capture, so this one is not parity
	["@lsp.type.payee"] = "@string",
	["@lsp.type.account"] = "@variable.member",
	["@lsp.type.amount"] = "@number",
	["@lsp.type.commodity"] = "@markup.raw",
}

-- `default` so a colorscheme that styles these groups itself works
local function link_tokens()
	for from, to in pairs(token_highlights) do
		vim.api.nvim_set_hl(0, from, { link = to, default = true })
	end
end

link_tokens()

-- A colorscheme clears the highlight table, taking the links with it
vim.api.nvim_create_autocmd("ColorScheme", {
	group = group,
	callback = link_tokens,
})

-- Start firepath on every ledger buffer
vim.api.nvim_create_autocmd("FileType", {
	group = group,
	pattern = "ledger",
	callback = function()
		if vim.fn.executable(bin) == 0 then
			vim.notify("firepath not built: run `just build` in " .. root, vim.log.levels.WARN)
			return
		end
		vim.lsp.start({
			name = "firepath",
			cmd = { bin, "lsp" },
			root_dir = root,
		})
	end,
})

-- Make firepath the only server on a ledger buffer
vim.api.nvim_create_autocmd("LspAttach", {
	group = group,
	callback = function(args)
		if vim.bo[args.buf].filetype ~= "ledger" then
			return
		end
		local client = vim.lsp.get_client_by_id(args.data.client_id)
		if client and client.name ~= "firepath" then
			vim.lsp.buf_detach_client(args.buf, client.id)
		end
	end,
})
