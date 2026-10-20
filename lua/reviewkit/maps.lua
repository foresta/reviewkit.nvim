--- Default keys as data: `plugin/` defines the <Plug> maps from it at load
--- without loading the orchestrator. Fields: mode, lhs, action, desc, and
--- whether the key is only mapped in the diff view's buffers.
return {
  { "n", "<leader>rc", "comment", "reviewkit: comment on this line" },
  { "x", "<leader>rc", "comment_range", "reviewkit: comment on the selection" },
  { "n", "<leader>rS", "suggest", "reviewkit: suggest a replacement for this line" },
  { "x", "<leader>rS", "suggest_range", "reviewkit: suggest a replacement for the selection" },
  { "n", "<leader>rm", "move_comment", "reviewkit: move a comment of this file here" },
  { "x", "<leader>rm", "move_comment_range", "reviewkit: move a comment of this file to the selection" },
  { "n", "<leader>re", "edit_comment", "reviewkit: edit the comment here" },
  { "n", "<leader>rx", "remove_comment", "reviewkit: remove the comment here" },
  { "n", "<leader>rd", "decision", "reviewkit: set the decision" },
  { "n", "<leader>rt", "thread", "reviewkit: show the thread on this line" },
  { "n", "<leader>rT", "threads", "reviewkit: list the pull request's threads" },
  { "n", "<leader>rq", "toggle_queue", "reviewkit: toggle the queue list" },
  { "n", "<leader>rv", "toggle_viewed", "reviewkit: toggle viewed for this file" },
  { "n", "<leader>rs", "submit_default", "reviewkit: submit to herdr" },
  { "n", "<leader>ro", "open_diff", "reviewkit: open the side-by-side view" },
  { "n", "]f", "next_file", "reviewkit: next target", true },
  { "n", "[f", "prev_file", "reviewkit: previous target", true },
  { "n", "]h", "next_hunk", "reviewkit: next hunk", true },
  { "n", "[h", "prev_hunk", "reviewkit: previous hunk", true },
  { "n", "]t", "next_thread", "reviewkit: next thread", true },
  { "n", "[t", "prev_thread", "reviewkit: previous thread", true },
  { "n", "]u", "next_unviewed", "reviewkit: next file not viewed yet", true },
}
