-- Project-local Neovim config for firepath, loaded via `exrc`
--
-- It starts the language server from the binary built from this repo
local root = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h")
local bin = root .. "/target/debug/firepath"

local group = vim.api.nvim_create_augroup("firepath", { clear = true })

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
