# reviewkit.nvim

A kit of small, composable parts for doing code review inside Neovim — reviewing
other people's pull requests and reviewing code written by AI coding agents with the
same workflow.

> Status: design phase. No code yet. See [`docs/architecture.md`](./docs/architecture.md),
> [`docs/tech-stack.md`](./docs/tech-stack.md) and [`docs/design.md`](./docs/design.md)
> (Japanese) for the design.

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

## Planned layout

```
cli/                  Rust: reviewkit CLI (core, source, sink, git/gh/herdr wrappers)
lua/reviewkit/
  cli.lua core.lua    thin shim: run the CLI, cache the session, fire User autocmds
  types.lua           LuaCATS types generated from the CLI's JSON schema
  view/               diff layout, annotations, queue list / input
  orchestrator.lua    :Review commands and default keymaps
```

## Requirements (planned)

- Neovim >= 0.12
- The `reviewkit` binary: install from GitHub Releases, or build with a Rust toolchain
  (`cargo build --release`; plugin managers can run this as a build step)
- `git`
- `gh` CLI, authenticated (for pull requests)
- [herdr](https://herdr.dev/) CLI (for sending to a coding agent)

macOS and Linux. Windows is not supported.

## Related

- [herdr-send-queue.nvim](https://github.com/foresta/herdr-send-queue.nvim) — the herdr
  transport this project grew out of.

## License

MIT
