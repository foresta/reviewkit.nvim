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
`chmod +x` it. The binary is looked up in this order: `setup({ cli = { path = "..." } })`, `$REVIEWKIT_BIN`,
the plugin's own `cli/target/{debug,release}` builds when `cli = { dev = true }`, the plugin's
`bin/reviewkit`, then `reviewkit` on `$PATH`. Run `:checkhealth reviewkit` to see which binary is used, whether it matches
the plugin, and whether `git`, `gh` and `herdr` are available.

## Usage

| Command | |
|---|---|
| `:Review diff <base> [<head>]` | review the working tree against `<base>`, or the commit `<head>` (shown read-only, whatever is checked out); `main...` compares with the merge-base, like `git diff main...`; `A..B` is `A B` (so it takes no `<head>`) |
| `:Review pr <number\|url>` | review a pull request |
| `:Review resume [id]` / `:Review refresh` | reopen a saved session (this repository's first) / recompute it |
| `:Review threads [refresh]` | list the pull request's review threads |
| `:ReviewSubmit herdr\|github\|clipboard` / `:'<,'>ReviewSubmit <sink>` | send the queue (the range form, in the queue list, sends only the selected comments); comments leave it only at the session's own destination (GitHub for a PR, the agent for a local diff), so a PR review can go to an agent first and to GitHub later; GitHub submits are previewed first (`sinks.github.confirm = false` skips it) |
| `:ReviewClose[!]` | close the session (`!` also deletes its state); asks first while comments are unsent |
| `:ReviewHistory` | list the comments that left the queue; `r` puts back one that `D` moved there (a comment that reached its destination stays, since editing it would deliver it twice) |
| `:ReviewInstall` | install or update the CLI (release download, else `cargo build`) |
| `:ReviewLog` | show recent CLI calls and errors |

To send only some comments, select their lines in the queue list (`<leader>rq`) and press
`<leader>rs`, or run `:'<,'>ReviewSubmit <sink>` there. A selection never carries the decision
or thread actions, so you can ask the agent to fix a few things and post the rest, decision
included, to GitHub later.

A comment leaves the queue only when it reaches the session's own destination: GitHub for a
pull request, the agent (herdr) for a local diff. Sending a PR review to the agent first keeps the
comments queued and marked as delivered (`→ herdr` in the queue list); `D` in the queue list moves
delivered comments to the history. Pass `consume = true` or `false` in the sink's options
(`setup({ sinks = { herdr = { consume = false } } })`) to change it; GitHub always consumes, since
keeping posted comments would post them twice.

Comments posted to GitHub end with a hidden HTML comment (`<!-- reviewkit:… -->`). If a submit is
interrupted, the next one uses it to find what already reached GitHub instead of posting it again.

In the comment window, a first word like `nit:` sets the comment's kind (`issue`, `suggestion`,
`question`, `praise`, `nit`; `issue` when omitted), and a ```` ```suggestion ```` block becomes a
suggested replacement. `:w` saves, `q` cancels.

Sessions are saved under `setup({ cli = { state_dir = "..." } })` if set, else
`$REVIEWKIT_STATE_DIR`, else `$XDG_STATE_HOME/reviewkit`, else `~/.local/state/reviewkit`. Pull
requests are checked out there too, never in your own working tree.

Default keys (disable with `setup({ set_keymaps = false })`; `<Plug>(reviewkit-*)` maps stay):
`<leader>rc` comment, `<leader>rS` suggest a replacement, `<leader>rm` move a comment of this file
here (all three also on a visual selection), `<leader>re` / `<leader>rx` edit / remove the comment here,
`<leader>rd` decision, `<leader>rv` toggle viewed, `<leader>rq` queue, `<leader>rt` / `<leader>rT`
thread here / thread list, `<leader>rs` submit to herdr (visual, in the queue list: submit the selected
comments), `<leader>ro` side-by-side view.
Inside the side-by-side view: `]f` / `[f` next / previous file, `]h` / `[h` next / previous hunk,
`]t` / `[t` next / previous thread, `]u` next file not viewed yet.

## Layout

```
cli/                  Rust: reviewkit CLI (core, source, sink, git/gh/herdr wrappers)
lua/reviewkit/
  cli.lua core.lua    thin shim: run the CLI, cache the session, fire User autocmds
  types.lua           LuaCATS types generated from the CLI's JSON schema
  view/               diff layout, annotations, queue list, input, threads, submit preview
  orchestrator.lua    :Review commands and default keymaps
  maps.lua            the default keymaps as data
  sink/clipboard.lua  a sink written in Lua
  install.lua         download or build the CLI
  health.lua log.lua  :checkhealth reviewkit, :ReviewLog
plugin/reviewkit.lua  commands and <Plug>(reviewkit-*) maps
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
