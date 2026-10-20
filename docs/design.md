# reviewkit.nvim — 詳細設計（契約）

Rust CLI `reviewkit` と Lua の間で受け渡すデータ、CLI のサブコマンド、Lua 側の公開 API を決める。
部品の責務は `docs/architecture.md`、言語とツールは `docs/tech-stack.md`。

## 1. 境界の原則

- **状態は CLI が持つ**。変更コマンドは、内容が変わらなくても更新後の `Session` 全体を返す。Lua はキャッシュを丸ごと置き換え、
  節ごとのハッシュ（`etags`）を比べて変わった節の `User` autocmd だけ発火する
- **Lua は Session を書き換えない**。読み取り（コメント可能判定、注釈、移動）はキャッシュで同期的に済ませ、
  CLI を呼ぶのは変更と refresh のときだけ
- **CLI は対話しない**。選択が要るとき（herdr の pane が複数）は `E_AMBIGUOUS` と候補を返し、
  Lua が `vim.ui.select` で選んで `--target` 付きで再実行する
- **呼び出しは常に非同期**（`vim.system` + callback）
- **enum は追加のみ**。Lua は未知の値を落とさず通す。同じ `schema_version` 内ではフィールドの追加しかしない
- Lua の core が同時に持つ Session は 1 つ。イベントの `data.session_id` は将来複数にするための印

## 2. 応答の封筒と終了コード

stdout は常に 1 つの JSON オブジェクト。stderr はログ。panic 等で JSON でなかったときは Lua が `E_INTERNAL` に変換する。

```json
{ "ok": true,  "data": <コマンド固有 | null>, "session": <Session。変更系コマンドでは常に> }
{ "ok": false, "error": { "code": "E_GH_AUTH", "message": "...", "hint": "run gh auth login", "retryable": false, "details": {} } }
```

Lua は `error.code` で分岐する。`details` は常にオブジェクト（無ければ `{}`）、`hint` は無ければ省略。終了コードはシェル利用者とテスト向け。

| 終了コード | 意味 | code |
|---|---|---|
| 0 | 成功（部分成功を含む） | |
| 1 | 引数誤り | `E_USAGE` |
| 2 | 検証 | `E_INVALID`、`E_NOT_COMMENTABLE` |
| 3 | 不在 | `E_NO_SESSION`、`E_NO_COMMENT`、`E_NO_TARGET`、`E_NO_THREAD` |
| 4 | 外部ツール | `E_GIT`、`E_GH`、`E_GH_AUTH`、`E_GH_MISSING`、`E_GH_PENDING_REVIEW`、`E_HERDR`、`E_HERDR_MISSING`、`E_AMBIGUOUS` |
| 5 | 競合 | `E_CONFLICT`、`E_LOCKED` |
| 6 | 契約不一致 | `E_SCHEMA_VERSION`（保存済み session.json が古い版のとき） |
| 70 | 内部 | `E_INTERNAL` |

- `E_NOT_COMMENTABLE` は「行がファイルの範囲外」か「path がリポジトリに無い」ときだけ。diff 外や targets 外の path への
  コメントは積める（D4）。その場合 `commentable.github = false` で、sink-github は review 本文の「Comments outside the diff」節に
  `path` と行、`blob/<sha>/path#L..` のリンク、本文を書いて送る（`Plan.body_comment_ids`）。stale だけは `skipped`。
  issue 以外の kind は本文の先頭に `**nit:**` のように付ける
- `E_AMBIGUOUS` は `details.candidates` に候補を載せる。cwd に一致する agent が 0 件なら全 agent を候補にする
- `E_GH_PENDING_REVIEW` は GitHub 側に pending review が残っていて `addPullRequestReview` が通らないとき
- `E_SCHEMA_VERSION` は CLI が古い session.json を読んだとき。CLI と `types.lua` の版違いは Lua が `version` で検出する

共通フラグ: `--state-dir`（既定 `$REVIEWKIT_STATE_DIR` > `$XDG_STATE_HOME/reviewkit` > `~/.local/state/reviewkit`）、
`--cwd`、`--log-level`、`--log-file`。ツールのパスは `REVIEWKIT_GIT` / `REVIEWKIT_GH` / `REVIEWKIT_HERDR` で上書きできる
（GUI 版 Neovim で PATH が引き継がれない場合とテスト用）。

## 3. 型

JSON、snake_case。`?` は省略可能な入力、`| null` は出力で null になりうる。
行番号は 1 始まり、範囲は閉区間。architecture.md の「queue」は `Session.comments` のこと。

```
Session {
  schema_version: 1
  id: string                -- "pr/<host>/<owner>/<repo>/<number>"
                            -- "local/<sha256(repo_root)[:12]>/<base_spec>..<head_spec>"（'/' は '__'）
  rev: int                  -- 変更ごとに +1
  state_path: string        -- Lua が fs_event で監視できる
  repo:   { root, host, owner, name, remote_url }     -- host / owner / name は local の非 GitHub なら null
  source: { kind: "pr", number, url, title, author, state: "OPEN|CLOSED|MERGED", head_ref, base_ref, base_oid, node_id, worktree }
        | { kind: "local", base_spec, head_spec: "worktree" | string, worktree }   -- worktree == repo.root
  base:   { ref: string|null, sha: string }            -- PR は merge-base。merged / closed は GitHub の base_oid から求める
  head:   { ref: string|null, sha: string|null, dirty: bool }   -- 作業ツリーなら sha は null
  targets: Target[]   comments: Comment[]   decision: Decision|null   threads: Thread[]
  thread_actions: { thread_id, action: "resolve" | "unresolve" }[]
  viewed: { [path]: { blob: sha, at } }   -- reviewkit 内だけの既読。GitHub には送らない
  last_submit: { sink, at, result: SinkResult } | null
  pending_submit: bool      -- 中断された GitHub 送信が残っている（読むたびに state dir から求める）
  history: Comment[]        -- 消費されてキューを出たコメント（deliveries 付き）
  etags: { targets, comments, decision, threads, thread_actions, viewed }   -- 節ごとの内容ハッシュ
  created_at, updated_at: RFC 3339
}

Target {                    -- 実ファイルは <source.worktree>/<path>。Lua が結合する
  id: "t_<n>"   path   old_path: string|null
  status: "modified|added|deleted|renamed|copied|type|untracked"
  reason: "changed" | "thread" | "impact"
  base_blob: sha|null   head_blob: sha|null   binary: bool   too_large: bool
  viewed: bool              -- Session.viewed の blob が head_blob と一致するとき true。ファイルが変わると自動で false
  stats: { added, removed }
  hunks: Hunk[]
  commentable: { base: [[start, end], ...], head: [[start, end], ...] }   -- GitHub の ±3 行。結合・クリップ済み
  range: { start, end } | null                                            -- reason が thread / impact のとき。
                                                                          -- diff 外のスレッドは head に存在するファイルだけ Target になる
}

Hunk { base_start, base_count, head_start, head_count }
                            -- 0 コンテキスト。count == 0 のとき start は git と同じく「直前の行」（0 は先頭より前）

Thread {                    -- 読み取り専用。GitHub から取り込んだ既存スレッド
  id, path, subject_type: "line" | "file"
  side: "base"|"head", start_side: "base"|"head"|null
  line: int|null, start_line: int|null, original_line: int|null   -- file スレッドと outdated は line が null
  resolved: bool, outdated: bool
  comments: { id, author, body, created_at, url }[]
}

Comment {
  id: "c_<ulid>"   path   side: "base"|"head"   line: int   start_line: int|null
                            -- path は side によらず Target.path（head 側の名前）。base 側の実体は Target.old_path で引く
  kind: "issue|suggestion|question|praise|nit"          -- 既定 issue
  body: string   suggestion: string|null                -- ```suggestion ブロック用。sink-github が整形する
  reply_to: thread_id|null   -- 返信はスレッドに付くので、行やファイルの存在を検査せず、再アンカーもしない
  anchor: { base_sha, head_sha: sha|null, blob: sha|null, context: { before: string[], lines: string[], after: string[] } }
                            -- 作業ツリー側は head_sha が null。代わりに blob（git hash-object）で固定する
  stale: bool
  commentable: { github: bool, reason: Reason|null }
  last_error: { sink, code, message, at } | null
  deliveries: { sink, at, mode: "sent" | "staged" | "copied", url? }[]
  created_at, updated_at
}
Reason = "outside_diff" | "stale" | "binary" | "not_supported"

CommentInput { path, side, line, start_line?, kind?, body, suggestion?, reply_to? }
                            -- core が anchor / commentable / id を埋める
CommentPatch { line?, start_line?, kind?, body?, suggestion? }
                            -- 省略は変更なし。start_line / suggestion は null で解除。line を変えると stale が消える

Decision { event: "approve|request_changes|comment", body, updated_at }
                            -- event は GitHub の PullRequestReviewEvent と 1:1
DecisionInput { event, body? }                        -- decision set の stdin。updated_at は core が付ける

PrInfo {                    -- 表示しないが保持する PR の付帯情報。Session には載せず pr show で取り出す
  title, body, author, url, created_at, updated_at
  timeline: { kind: "comment" | "review" | "commit" | "event", author, body: string|null, state: string|null, created_at, url }[]
  checks: { name, status, conclusion: string|null, url: string|null }[]
  fetched_at
}

SubmitOptions {             -- submit の stdin（省略可）
  target?: string           -- herdr の pane_id
  agent_type?: "claude" | "plain"
  header?: string
  stage?: bool              -- herdr: 送信せず入力欄に置くだけ（pane send-text）
  match_foreground_cwd?: bool
  consume?: bool            -- 送れたコメントをキューから消すか。既定は local の herdr だけ。github は常に消す
}

SinkResult {
  sink   at   url: string|null
  sent:    { comment_id, remote_id?, url?, version? }[]  -- version: 渡したときのコメントの updated_at
  failed:  { comment_id|null, error: Error }[]          -- null は review 全体の失敗
  skipped: { comment_id, reason: Reason }[]
  thread_actions: { thread_id, action, status: "sent" | "failed", error?: Error }[]
  decision: "sent|failed|skipped|not_applicable"
  summary: string                                       -- "3 sent, 1 skipped (outside diff), 0 failed"
  mode: "sent" | "staged" | "copied"                    -- 既定 sent。staged / copied は消費しない
  attempted?: comment_id[]                              -- 試行に含めたコメント。省略は全件
}
```

- `side` は architecture の語彙（base / head）。GitHub の `LEFT` / `RIGHT`、`APPROVE` 等への変換は sink-github の内側だけで行う
- SinkResult の適用（D4）: `sent` のコメントには `deliveries` に `{ sink, at, mode, url? }` を足す。キューから消して
  `Session.history` へ移す（消費する）のは、`SinkResult.mode = sent` かつ sink がセッション種類の主な送り先
  （PR = github、local = herdr。`SubmitOptions.consume` で上書き。github は二重投稿を避けるため常に消費）のときだけ。
  送れたコメントの `last_error` は消し、全体失敗の印はその sink へ送り済みのコメントと `attempted` 外のコメントには付けない。
  `sent[].version` がコメントの今の `updated_at` と違う（送信中に編集された）ものは送れなかった扱いでキューに残し、結果からも外す。herdr の stage は `staged`、clipboard は
  `copied` で、消費しない。herdr は自分へ `sent` 済みのコメントを再送しない。本文・suggestion・kind を変えると `deliveries` は消える。
  `failed` は `last_error` を付けて残す。`skipped` はそのまま。`thread_actions` は `sent` のものだけ消す（PR の refresh でも、
  スレッドが消えたか GitHub 側で既にその状態になったものは消す）。`decision` は `sent` かつ消費のときだけ消す。`last_submit` に結果を記録する
- sink-github で Decision が無く comments がある場合は `event = comment` で送る（行コメントは review の一部としてしか送れない）。本文は diff の外のコメントの節だけ（無ければ空）
- sink-herdr のプロンプトは header、Decision の本文（あれば）、番号付きのコメント（`@path#L1-L2` 形式。`plain` は `path:1-2`）の順。
  Decision の event は送れないので、本文が空の Decision は `skipped` として残る

## 4. CLI サブコマンド

本文を含む入力は stdin JSON、スカラーはフラグ。変更系は封筒の `session` を返す。

```
reviewkit version                                        -> { version, schema_version }
reviewkit health [--cwd]                                 -> { tools: { git, gh, herdr }, gh_auth, state_dir }
reviewkit schema                                         -> JSON Schema（全型）

reviewkit session open --pr <number|url> [--cwd]
                                                         -> session   gh でメタ取得、bare mirror + worktree、targets、threads
                                                            番号は --cwd の origin のリポジトリ。URL ならそのリポジトリ（origin と違ってもよい）
reviewkit session open --local [--base REV] [--head REV|worktree] [--cwd]
                                                         -> session   既存があれば再開して refresh。--base A... は A と head の
                                                                      merge-base（refresh ごとに求め直す）。A...B で --head を省くと head = B
reviewkit session open --session ID                      -> session   再開
reviewkit session get     --session ID                   -> session
reviewkit session list    [--cwd]                        -> data: SessionSummary[]
reviewkit session refresh --session ID [--path P ...]    -> session   Source 再実行。base / head が動いたら再アンカー。PR は pr.json も取り直す
                                                                     --path は将来の部分更新用で、現状は受け取るだけで全体を再計算する
reviewkit session close   --session ID [--purge]         -> data: { closed: true }   purge で worktree と state を消す

reviewkit target list --session ID                       -> data: Target[]
reviewkit target viewed --session ID --path P [--undo]   -> session   既読の付け外し。head_blob を一緒に記録する
reviewkit pr show --session ID                           -> data: PrInfo   pr.json から。無ければ E_NO_SESSION ではなく data: null
reviewkit file base   --session ID --path P              -> data: { exists, binary, too_large, lines: string[]|null }
                                                                     targets 外の path も可。base で未追跡なら exists = false

reviewkit comment add    --session ID  (stdin: CommentInput)         -> session
reviewkit comment update --session ID --id CID (stdin: CommentPatch) -> session
reviewkit comment remove --session ID --id CID                       -> session
reviewkit comment list   --session ID [--path P]                     -> data: Comment[]

reviewkit decision set   --session ID (stdin: DecisionInput)  -> session
reviewkit decision clear --session ID                    -> session

reviewkit sink list                                      -> data: { name, available, reason }[]
reviewkit sink targets --sink herdr [--cwd]              -> data: { agents[], panes[] }   picker 用
reviewkit submit --session ID --sink github|herdr [--dry-run] [--options -]
                                                         -> data: SinkResult（dry-run は data.preview）, session
                                                            --options - のときだけ stdin から SubmitOptions を読む
reviewkit submit apply --session ID (stdin: SinkResult の sent / failed / skipped / thread_actions / decision)
                                                         -> session   Lua 側 sink の結果を core に適用。at / summary は core が補う

reviewkit thread list    --session ID [--refresh]        -> data: Thread[]   --refresh は session 全体を取り直し、session も返す
reviewkit thread resolve --session ID --thread TID [--undo] -> session       GitHub 上の状態と同じになる操作は積まずに消す
```

- `--dry-run` は sink が送る内容そのもの（GraphQL の変数 / herdr のプロンプト文字列）を返す。整形の反復と view のプレビューに使う
- `submit` は送信前の失敗（未認証、pane 未解決）だけ `ok: false`。リクエストを送った後は部分失敗でも `ok: true` と SinkResult

## 5. 状態の所有と並行性

- 保存先は `<state-dir>/sessions/<id>/session.json`。PR の付帯情報（conversation、checks）は同じディレクトリの `pr.json` に分け、
  Session の封筒を重くしない。変更系は advisory lock → 読む → 適用 → `rev += 1` → temp + rename。
  ロックはプロセス終了で解放される。CLI のロック待ちは短く（2 秒）、Lua の `cli.call` が `E_LOCKED` を間隔を空けて数回再試行する。
  PR の refresh（gh と git fetch）はロックの外で取得し、ロックの中では読み直したセッションに当てるだけにする。
  GitHub への送信は、送信中の編集を失わないよう今はロックを持ったまま行う（版の照合で外せるようになるまで）。
  PR の取得はセッションごとの fetch.lock で直列にし、リポジトリごとの mirror は `mirror.lock` で守る（作成は一時ディレクトリから rename）。
  git の fetch / clone は `REVIEWKIT_GIT_TIMEOUT_MS`（既定 120 秒）で打ち切り、認証のプロンプトは出さない
- PR の実体化は `<state-dir>/repos/<key>/mirror.git`（bare。ローカルからの clone でオブジェクトはハードリンク）と
  `<state-dir>/repos/<key>/pr/<n>/`（worktree）。2 回目以降は同じ worktree を使い回し、head が同じなら fetch しない。
  ユーザーのリポジトリには何も書かない
- 複数 Neovim で同じ Session を開いたとき、Lua は常に返ってきた Session でキャッシュを置き換える
  （コメント単位の last-writer-wins）。他インスタンスの変更は `state_path` の `vim.uv.new_fs_event`
  （150 ms デバウンス）→ `session get` → etags 差分でイベント発火。`--expect-rev N` で `E_CONFLICT` を返す
  CAS は定義だけして最初は使わない
- 二重送信の防止: gh を呼ぶ前に `pending_submit.json` を書く。次の GitHub への submit で残っていれば threads を取り直し、
  「同じファイル・side に、自分が、同じ本文で、試行の開始以後に投稿したもの」を送信済みとみなしてから消す（行は push で動くので見ない）。
  review 本文に書いたコメントは、自分の review が送った本文のまま投稿されていれば送信済みとみなす。
  残っている間は Session の `pending_submit` が true（ファイルの有無から毎回求める）で、Lua は開いたときに知らせる
- GitHub Enterprise は `repo.host` を gh の `--hostname` に渡す

## 6. hunk・コメント可能範囲・行ずれ

- `session open` / `refresh` で CLI が計算し、`Target.hunks` と `Target.commentable` に載せる。
  view は再計算せず、キャッシュで同期的に判定する
- local の再計算: orchestrator が worktree と `.git` を `vim.uv.new_fs_event` で監視し（デバウンス）、`BufWritePost` と
  `FocusGained` でも `refresh` を呼ぶ。エージェントは Neovim の外でファイルを書くため、バッファの保存だけでは足りない。
  `base_spec` が `HEAD` のように動く指定なら、commit で base も動くので `refresh` が base 側も追従する
- `comment add` で `anchor` に対象行と前後 2 行のテキストを保存する。作業ツリー側の blob は `git hash-object -w` で
  object store に書く（後で差分を取るため。dangling object として `git gc` が回収する）
- `refresh` で side の sha（作業ツリーなら blob）が変わったとき: `git diff -U0 <old> <new> -- path` の hunk で、
  上にある分だけ `line` をずらす。ずらした先の行テキストが `anchor.context.lines` と一致すれば追従する。範囲が hunk と
  交差していても、周囲の edit 分だけずらした位置に `context`（before / lines / after）がそのまま見つかれば追従する
  （末尾改行の追加など）。見つからなければ `stale`。追従できた時点で `stale` は解ける。blob が同じなら何もしない。
  base 側のコメントも同じ規則で base の変化に追従する。rename された path のコメントは新しい path に付け替える。
  blob の差分が取れないとき（blob が無い、古い blob が gc で消えた）は、今の位置の行テキストと `anchor.context.lines` の一致だけで判定する
- `stale` の扱い: view は薄く表示、sink-github は `skipped(stale)`、sink-herdr は「行がずれた可能性あり」を添えて送る。
  `comment update` で `line` を付け直すと解消する

## 7. Lua 側

### モジュール

```
lua/reviewkit/
  init.lua            setup(opts)
  config.lua          既定値
  cli.lua             バイナリ解決、run(args, { stdin? }, cb(err, envelope))、封筒の解釈、version 照合
  core.lua            state を持つ唯一の Lua モジュール。キャッシュ、etags 差分で User autocmd、state_path の監視、Lua 側 sink の登録
  types.lua           生成された LuaCATS + SCHEMA_VERSION
  health.lua          :checkhealth reviewkit
  install.lua         ensure()（Releases から取得して検証、合わなければ cargo build → <plugin>/bin/）
  view/diff.lua       タブ・左右ウィンドウ・次 / 前の移動。base 側は file base で取得した読み取り専用バッファ。
                      左右の位置合わせと色付けは Neovim の diff モード（diffthis）。hunk 間の移動とコメント可否は Target から
  view/annotate.lua   extmark の注釈（stale は薄く）
  view/queue.lua      一覧（quickfix 既定、float も可）
  view/input.lua      入力窓
  view/thread.lua     カーソル行のスレッドを float で表示。r で返信（reply_to 付きコメント）、R で resolve の切り替え
  orchestrator.lua    :Review の配線、worktree の監視、既定キーマップ
  sink/clipboard.lua  Lua 側 sink の最小例
plugin/reviewkit.lua  コマンド定義だけ
scripts/gen-types.lua `reviewkit schema` の JSON Schema から types.lua を生成
```

### core の公開 API

CLI を呼ぶものは非同期 `cb(err, result)`、キャッシュ読みは同期。返るテーブルは読み取り専用で、書き換えてはならない。

```
core.open(spec, cb)          spec = { kind = "pr", number = 123 } | { kind = "local", base = "HEAD~3", head? }
core.resume(id, cb)
core.refresh(opts, cb)
core.close(opts, cb)
core.session() / core.targets() / core.comments() / core.decision() / core.threads()
core.is_commentable(path, side, line)      -- Target.commentable から
core.add_comment(input, cb) / core.update_comment(id, patch, cb) / core.remove_comment(id, cb)
core.set_decision(decision, cb) / core.clear_decision(cb)
core.set_viewed(path, on, cb)
core.resolve_thread(id, resolved, cb) / core.refresh_threads(cb) / core.pending_thread_action(id)
core.pr_info(cb)               -- PrInfo。表示は持たないが取り出せる
core.submit(sink, opts, cb)  -- Rust 側 sink は submit。Lua 登録 sink は impl.submit(session, opts, cb) の結果を submit apply に渡す
core.register_sink(name, impl)
core.on(event, fn)           -- User autocmd の薄い包み
```

### イベント

`User` autocmd。`data = { session_id, rev }`。etags の差分で発火するので、1 応答で複数発火することも、無変更なら発火しないこともある。

`ReviewSessionOpened`、`ReviewTargetsChanged`、`ReviewQueueChanged`、`ReviewDecisionChanged`、`ReviewThreadsChanged`
（threads と thread_actions の両方）、`ReviewViewedChanged`、`ReviewSubmitted`、`ReviewSessionClosed`

### 設定とコマンド

```lua
require("reviewkit").setup({
  cli = { path = nil, dev = false },
  tools = { git = nil, gh = nil, herdr = nil },   -- REVIEWKIT_* として CLI に渡す
  set_keymaps = true,
  view = { queue = "quickfix" },                  -- "quickfix" | "float"
  sinks = {
    herdr = { agent_type = "claude", header = nil, match_foreground_cwd = true },   -- SubmitOptions に詰める
    github = { confirm = true },                    -- :ReviewSubmit github は送る内容を見せて確認する
    clipboard = { register = "+" },
  },
})
```

`:Review pr <number|url>`、`:Review diff <base> [<head>]`、`:Review resume`、`:Review refresh`、`:Review threads [refresh]`、
`:ReviewSubmit herdr|github`、`:ReviewClose`、`:ReviewInstall`、`:ReviewLog`。`setup()` を呼ばなくてもコマンドは使える。

## 8. 未決事項

- `anchor.context` で ±N 行を探索して `stale` を自動で解くか
- GitHub の suggestion ブロックの送信形式
- `--expect-rev` を view のどの操作で使うか
- 未保存バッファの内容を stdin で渡す `refresh --content -`
- source-graph は同じ CLI のサブコマンドか、別バイナリ `reviewkit-graph` か
