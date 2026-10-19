# reviewkit.nvim

A kit of small, composable parts for doing code review inside Neovim — reviewing
other people's pull requests and reviewing code written by AI coding agents with the
same workflow.

> Status: early development (0.x). The contract between the CLI and the plugin may
> still change between minor versions. Design notes (Japanese):
> [`docs/architecture.md`](./docs/architecture.md), [`docs/tech-stack.md`](./docs/tech-stack.md),
> [`docs/design.md`](./docs/design.md).

## Concept

Review is split into parts that only talk to each other through a small core:

```
Source (pick what to read)  →  Core (session, queue, decision)  →  Sink (send)
   local diff / PR                     ↑ events                    herdr agent / GitHub review
                                      View (diff layout, annotations, queue)
```

- **Source** turns a local diff or a pull request into review targets.
- **Core** holds the session: targets, a queue of line-anchored comments, and the
  review decision (approve / request changes / comment).
- **View** shows the targets side by side and lets you stack comments on lines.
- **Sink** sends the queue somewhere: as one prompt to a coding agent running on
  [herdr](https://herdr.dev/), or as one review request to a GitHub pull request.

Each part can be replaced or evolved on its own. Future sources (for example, picking up
the code affected by a change via a dependency graph) plug into the same core.

## How it is built

The core, sources and sinks live in a **Rust CLI** (`reviewkit`) that speaks JSON over
stdin/stdout and owns the session state. The **Lua** side is the Neovim frontend: the
diff view, the comment queue UI, and the `:Review` commands. Neovim never mutates the
session directly; it calls the CLI and redraws from the session it gets back.

Because the CLI knows nothing about Neovim, the same binary can drive a review from
other frontends (an editor, a script, a coding-agent skill).

## Requirements

- Neovim >= 0.12, macOS or Linux (Windows is not supported)
- `git`
- `curl` and one of `sha256sum` / `shasum` / `openssl` to download the prebuilt CLI,
  or a Rust toolchain to build it
- `gh` CLI, authenticated, for pull requests
- [herdr](https://herdr.dev/) CLI, for sending to a coding agent

## Install

The plugin needs the `reviewkit` binary. `require("reviewkit.install").ensure()` puts one
in `<plugin>/bin/`: it downloads the release matching the plugin's version and verifies its
SHA-256, and falls back to `cargo build --release` when no release fits (for example on an
unreleased commit). `:ReviewInstall` does the same from inside Neovim.

Pinning the plugin to release tags gives you prebuilt binaries; tracking the default branch
builds with cargo when it is available.

lazy.nvim:

```lua
{
  "foresta/reviewkit.nvim",
  version = "*",
  build = function()
    local ok, err = require("reviewkit.install").ensure()
    if not ok then error(err) end
  end,
  opts = {},
}
```

dein.vim (TOML):

```toml
[[plugins]]
repo = "foresta/reviewkit.nvim"
build = "nvim --headless -l scripts/install.lua"
hook_add = """
lua require("reviewkit").setup({})
"""
```

vim.pack (Neovim 0.12):

```lua
vim.api.nvim_create_autocmd("PackChanged", {
  callback = function(ev)
    if ev.data.spec.name == "reviewkit.nvim" and ev.data.kind ~= "delete" then
      if not ev.data.active then
        vim.cmd.packadd("reviewkit.nvim")
      end
      local ok, err = require("reviewkit.install").ensure()
      if not ok then
        vim.notify(err, vim.log.levels.ERROR)
      end
    end
  end,
})
vim.pack.add({ { src = "https://github.com/foresta/reviewkit.nvim", version = vim.version.range("*") } })
require("reviewkit").setup({})
```

Release assets are plain executables (`reviewkit-<target>`); after downloading one by hand,
`chmod +x` it. The binary can also be set explicitly with `setup({ cli = { path = "..." } })` or
`$REVIEWKIT_BIN`. Run `:checkhealth reviewkit` to see which binary is used, whether it matches
the plugin, and whether `git`, `gh` and `herdr` are available.

## Usage

| Command | |
|---|---|
| `:Review diff <base> [<head>]` | review the working tree (or `<head>`) against `<base>`; `main...` compares with the merge-base, like `git diff main...` |
| `:Review pr <number\|url>` | review a pull request |
| `:Review resume [id]` / `:Review refresh` | reopen a saved session / recompute it |
| `:Review threads [refresh]` | list the pull request's review threads |
| `:ReviewSubmit herdr\|github\|clipboard` | send the queue |
| `:ReviewClose[!]` | close the session (`!` also deletes its state) |

Default keys (disable with `setup({ set_keymaps = false })`; `<Plug>(reviewkit-*)` maps stay):
`<leader>rc` comment, `<leader>re` / `<leader>rx` edit / remove the comment here,
`<leader>rd` decision, `<leader>rv` toggle viewed, `<leader>rq` queue, `<leader>rt` / `<leader>rT`
thread here / thread list, `<leader>rs` submit to herdr, `<leader>ro` side-by-side view.
Inside the side-by-side view: `]f` / `[f` next / previous file, `]h` / `[h` next / previous hunk.

## Layout

```
cli/                  Rust: reviewkit CLI (core, source, sink, git/gh/herdr wrappers)
lua/reviewkit/
  cli.lua core.lua    thin shim: run the CLI, cache the session, fire User autocmds
  types.lua           LuaCATS types generated from the CLI's JSON schema
  view/               diff layout, annotations, queue list, input, threads
  orchestrator.lua    :Review commands and default keymaps
  install.lua         download or build the CLI
```

## Development

`make test` (Rust + Lua stub + real-binary tiers), `make check` (fmt, clippy, stylua,
lua-language-server, types drift), `make gen-types`, `make dist` (a release binary for the
host). Releases are cut by bumping `[workspace.package] version` from `X.Y.Z-dev` to `X.Y.Z`,
tagging `vX.Y.Z` on that commit, then bumping to the next `-dev`.

## Related

- [herdr-send-queue.nvim](https://github.com/foresta/herdr-send-queue.nvim) — the herdr
  transport this project grew out of.

## License

MIT
