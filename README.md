# reviewkit.nvim

A kit of small, composable parts for doing code review inside Neovim — reviewing
other people's pull requests and reviewing code written by AI coding agents with the
same workflow.

> Status: design phase. No code yet. See [`docs/architecture.md`](./docs/architecture.md)
> (Japanese) for the architecture.

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

## Planned layout

```
lua/reviewkit/
  core/     session, targets, queue, decision, events
  lib/      git, gh, herdr wrappers
  source/   local, pr
  view/     diff layout, annotations, queue list / input
  sink/     herdr, github
```

## Requirements (planned)

- Neovim >= 0.12
- `git`
- `gh` CLI, authenticated (for pull requests)
- [herdr](https://herdr.dev/) CLI (for sending to a coding agent)

## Related

- [herdr-send-queue.nvim](https://github.com/foresta/herdr-send-queue.nvim) — the herdr
  transport this project grew out of.

## License

MIT
