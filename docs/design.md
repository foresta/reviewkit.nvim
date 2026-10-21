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
- **入力は厳格、保存は寛容**。stdin の JSON と sink のオプションに知らないキーや enum 値があれば `E_INVALID`
  （`details.unknown_fields` にパス）。古い CLI が新しいオプションを黙って捨て、頼んだのと違うものを送らないため。
  session.json などの保存ファイルの読み込みは寛容なまま（新しい CLI が書いたものを古い CLI が読めるように）。
  `submit apply` も寛容に読み、知らないキーはログに出す（Lua の sink が配送した後の報告なので、拒否すると記録が残らず再送で二重に届く。
  版のずれは先に契約の revision の照合で止まる）
- Lua の core が同時に持つ Session は 1 つ。イベントの `data.session_id` は将来複数にするための印

## 2. 応答の封筒と終了コード

stdout は常に 1 つの JSON オブジェクト。stderr はログ。CLI の panic も `E_INTERNAL` の封筒で返す。それでも JSON でなかったとき
（強制終了など）は Lua が `E_INTERNAL` に変換する。

```json
{ "ok": true,  "data": <コマンド固有 | null>, "session": <Session。変更系コマンドでは常に> }
{ "ok": false, "error": { "code": "E_AUTH", "message": "...", "hint": "run gh auth login", "tool": "gh", "retryable": false, "details": {} } }
```

Lua は `error.code` で分岐する。`details` は常にオブジェクト（無ければ `{}`）、`hint` は無ければ省略。終了コードはシェル利用者とテスト向け。

| 終了コード | 意味 | code |
|---|---|---|
| 0 | 成功（部分成功を含む） | |
| 1 | 引数誤り | `E_USAGE` |
| 2 | 検証 | `E_INVALID`、`E_NOT_COMMENTABLE` |
| 3 | 不在 | `E_NO_SESSION`、`E_NO_COMMENT`、`E_NO_TARGET`、`E_NO_THREAD` |
| 4 | 外部ツール | `E_GIT`、`E_TOOL`、`E_TOOL_MISSING`、`E_AUTH`、`E_PENDING_REVIEW`、`E_AMBIGUOUS` |
| 5 | 競合 | `E_CONFLICT`、`E_LOCKED` |
| 6 | 契約不一致 | `E_SCHEMA_VERSION`（保存済み session.json を移行できないとき） |
| 70 | 内部 | `E_INTERNAL`（バグ） |
| 74 | 入出力 | `E_IO`（ディスクフル、権限不足など環境の問題） |

- `E_NOT_COMMENTABLE` は「行がファイルの範囲外」か「path がリポジトリに無い」ときだけ。diff 外や targets 外の path への
  コメントは積める（D4）。その場合 `commentable.github = false` で、sink-github は review 本文の「Comments outside the diff」節に
  `path` と行、`blob/<sha>/path#L..` のリンク、本文を書いて送る（`SubmitPreview.body_comment_ids`）。stale だけは `skipped`。
  issue 以外の kind は本文の先頭に `**nit:**` のように付ける
- `E_AMBIGUOUS` は `details.candidates` に候補（`SinkTarget[]`）を載せる。cwd に一致する agent が 0 件なら全 agent を候補にする
- コードは「何が起きたか」を表し、ツール名を含めない。どのツールかは `error.tool`（"git" / "gh" / "herdr"）で示すので、
  sink や source を足してもコードは増えない。最初のリリース以降、コードは追加だけで改名しない。`E_GIT` は「git が動いて
  失敗した」ときに限り（core が直接依存するため残す）、git が無いときは他のツールと同じ `E_TOOL_MISSING`
- `retryable` は「同じコマンドをそのまま再実行しても副作用が重複しない」ときだけ true。GitHub への mutation と herdr への
  送信が失敗・timeout したときは、相手に届いたかどうか分からないので false にし、`details.outcome = "unknown"` と
  確かめ方の hint を付ける。GitHub は次の submit が pending の記録と照合する（§5）。herdr には照合が無いので、
  エージェントの画面で確かめてから送り直す
- private リポジトリの PR を、origin が別のリポジトリの checkout から開くと、reviewkit は `https://<host>/<owner>/<name>.git`
  を自分で組み立てて mirror を作る。この URL の fetch に限り、その host の credential helper を gh（`gh auth git-credential`）に
  差し替える。ユーザーの remote と設定には触れない。認証に失敗したら `E_AUTH`（tool = git、hint `gh auth login --hostname <host>`）
- `E_PENDING_REVIEW` は GitHub 側に pending review が残っていて `addPullRequestReview` が通らないとき
- `E_SCHEMA_VERSION` は CLI が移行できない session.json を読んだとき（移行の手順が無い古い版、または新しい版）。
  古い版でも移行の手順があれば読むときに移行するので、このエラーにはならない。CLI と `types.lua` の版違いは Lua が `version` で検出する

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
                            -- "local/<sha256(repo_root)[:12]>/<sha256(base_spec + "\0" + head_spec)[:16]>"
                            -- spec は source に読める形で残る。同じ id で spec が違えば E_CONFLICT
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
  pending_apply: bool       -- Lua の sink が配送した結果が、ロックを待って記録されていない（同上）
  history: Comment[]        -- キューを出たコメント（deliveries 付き）。宛先に届いて消費されたものと、`comment clear` で
                            -- 移しただけのもの。保存はするが封筒では常に空。
                            -- `history list` で取る。200 件を超えたら、消費済み（宛先に届いた）ものから古い順に捨てる。
                            -- `comment clear` で移しただけのものは 1000 件を超えた分だけ古い順に捨てる
  etags: { targets, comments, decision, threads, thread_actions, viewed }   -- 節ごとの内容ハッシュ
  created_at, updated_at: RFC 3339
}

Target {                    -- 実ファイルは <source.worktree>/<path>。Lua が結合する
  id: "t_<n>"   path   old_path: string|null
  status: "modified|added|deleted|renamed|copied|type|untracked"
  reason: "changed" | "thread" | "impact"
  base_blob: sha|null   head_blob: sha|null   binary: bool   too_large: bool
                            -- too_large: base 側・head 側のどちらかのファイル、または差分が 1 MiB を超える。
                            -- そのとき hunks は空で、View は diff をかけない（片側だけ全文で diff すると全行が
                            -- 追加に見える）。`file base` / `file head` は同じ 1 MiB を自分の側のサイズで判定し、
                            -- 超えれば読まずに lines を返さない。サイズは git の blob（作業ツリーはファイル）から取る
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
                            -- 行に置けるかの判定（Target.commentable の範囲、この github と reason の
                            -- outside_diff）は全体が GitHub の規則（差分の行と前後 3 行）。ホスティング先が
                            -- GitHub だけなので、規則を 1 つに揃えている。herdr と clipboard は範囲の制約を持たず、
                            -- view が local セッションで示す範囲も GitHub の目安（範囲外のコメントも積める、D4）。
                            -- 名前を中立にしないのは、別のホスティング先を足すときに範囲・可否・reason を
                            -- 送り先ごとに持つ構造へ作り直す必要があり（schema_version を上げる契約の変更）、
                            -- そのとき今の値が GitHub のものだと読めるようにするため
  last_error: { sink, code, message, at } | null
  deliveries: { sink, at, mode: "sent" | "staged" | "copied", url?, consumed }[]   -- consumed: この配送でキューを出た
  created_at, updated_at
}
Reason = "outside_diff" | "stale" | "binary" | "not_supported" | "delivered"
                            -- Comment.commentable.reason は outside_diff / binary だけ（stale は Comment.stale）。
                            -- stale / not_supported / delivered は SinkResult.skipped の reason にだけ出る

CommentInput { path, side, line, start_line?, kind?, body, suggestion?, reply_to? }
                            -- core が anchor / commentable / id を埋める
CommentPatch { line?, start_line?, kind?, body?, suggestion? }
                            -- 省略は変更なし。start_line / suggestion は null で解除。line を変えると stale が消える

Decision { event: "approve|request_changes|comment", body, updated_at, deliveries?: Delivery[] }
                            -- deliveries は Comment と同じ。decision set で作り直すと消える
                            -- event は GitHub の PullRequestReviewEvent と 1:1
DecisionInput { event, body? }                        -- decision set の stdin。updated_at は core が付ける

PrInfo {                    -- 表示しないが保持する PR の付帯情報。Session には載せず pr show で取り出す
  title, body, author, url, created_at, updated_at
  timeline: { kind: "comment" | "review" | "commit" | "event", author, body: string|null, state: string|null, created_at, url }[]
  checks: { name, status, conclusion: string|null, url: string|null }[]
  fetched_at
}

SubmitOptions {             -- submit の stdin（省略可）。sink 固有の項目を同じ階層に並べる
  consume?: bool            -- 送れたコメントをキューから消すか。既定は sink が決める（Sink::consumes）
  comment_ids?: string[]    -- 送るコメントを選ぶ。省略はキュー全体。選んだときは decision とスレッド操作を送らない
                            -- （decision は skipped）。キューに無く history にある id は送信済みとして落とす。
                            -- どちらにも無い id は E_NO_COMMENT、`[]` は E_INVALID
  expect_digest?: string    -- 確認したプレビューの `digest`。送る内容がそれと違えば送らずに E_CONFLICT
                            -- （details.reason = "preview_changed"）。digest を返さない sink に渡すと E_INVALID。
                            -- 共通の予約語は consume・comment_ids・expect_digest の 3 つ
  ...HerdrOptions | GithubOptions
}
HerdrOptions {
  target?: string           -- SinkTarget.id（pane）。無ければ cwd で探す
  agent_type?: "claude" | "plain"
  header?: string
  stage?: bool              -- エージェントの入力欄に置くだけで Enter は送らない（pane send-text）。git の staging とは無関係
  match_foreground_cwd?: bool
}
GithubOptions {}

SinkResult {
  sink   at   url: string|null
  sent:    { comment_id, remote_id?, url?, version? }[]  -- version: 渡したときのコメントの updated_at
  failed:  { comment_id|null, error: Error }[]          -- null は review 全体の失敗
  skipped: { comment_id, reason: Reason }[]
  thread_actions: { thread_id, action, status: "sent" | "failed", error?: Error }[]
  decision: "sent|failed|skipped|not_applicable"
  decision_version?: string                             -- 渡したときの Decision.updated_at
  summary: string                                       -- "3 sent, 1 skipped (outside diff), 0 failed"
  mode: "sent" | "staged" | "copied"                    -- 既定 sent。staged / copied は消費しない
  attempted?: comment_id[]                              -- 試行に含めたコメント。省略は全件
}
```

- `side` は architecture の語彙（base / head）。GitHub の `LEFT` / `RIGHT`、`APPROVE` 等との変換は lib/gh（取り込み）と
  sink-github（送信）の内側だけで行い、契約には出さない
- SinkResult の適用（D4）: `sent` のコメントには `deliveries` に `{ sink, at, mode, url? }` を足す。キューから消して
  `Session.history` へ移す（消費する）のは、`SinkResult.mode = sent` かつ sink がセッション種類の主な送り先
  （PR = github、local = herdr。`SubmitOptions.consume` で上書き。github は二重投稿を避けるため常に消費）のときだけ。
  判断は各 sink の `Sink::consumes` が持ち、core は sink の名前を知らない。`submit apply`（Lua の sink）は `--consume` を
  付けたときだけ消費する。Lua は `LuaSink.consumes(session, requested)`（requested は `opts.consume`）があればそれで、
  無ければ `opts.consume`（未指定は false）で決める。Lua の sink も、自分が sent で配送済みのコメントと decision は送らない（decision は `skipped` と報告する）。
  送れたコメントの `last_error` は消し、全体失敗の印はその sink へ送り済みのコメントと `attempted` 外のコメントには付けない。
  `sent[].version` がコメントの今の `updated_at` と違う（送信中に編集された）ものは送れなかった扱いでキューに残し、結果からも外す。herdr の stage は `staged`、clipboard は
  `copied` で、消費しない。herdr は自分へ `sent` 済みのコメントを再送しない。本文・suggestion・kind を変えると `deliveries` は消える。
  `failed` は `last_error` を付けて残す。`skipped` はそのまま。`thread_actions` は `sent` のものだけ消す（PR の refresh でも、
  スレッドが消えたか GitHub 側で既にその状態になったものは消す）。`decision` は `sent` なら `Decision.deliveries` に記録し、消費のときだけ消す。`SinkResult.decision_version`
  （渡した時点の `Decision.updated_at`）が今と違えば、記録も消去もしない。herdr は自分へ `sent` 済みの decision を再送しない。
  `last_submit` に結果を記録する
- sink-github は GitHub へ `sent` 済みのコメントを送らない（`skipped`、reason = `delivered`）。履歴から戻したコメントが
  再投稿されないため
- sink-github で Decision が無く comments がある場合は `event = comment` で送る（行コメントは review の一部としてしか送れない）。本文は diff の外のコメントの節だけ（無ければ空）
- sink-herdr のプロンプトは header、Decision の本文（あれば）、番号付きのコメント（`@path#L1-L2` 形式。`plain` は `path:1-2`）の順。
  Decision の event は送れないので、本文が空の Decision は `skipped` として残る。
  PR のセッションでは header の次に、PR の番号と URL、head のブランチと commit、その commit を展開した worktree の場所を書く
  （コメントの行番号はその commit のもので、エージェントの checkout とは限らない）。送り先のエージェントの checkout の HEAD が
  PR の head と違えば、プロンプトにそう書き、結果の `summary` にも添える

## 4. CLI サブコマンド

本文を含む入力は stdin JSON、スカラーはフラグ。変更系は封筒の `session` を返す。

```
reviewkit version                                        -> { version, schema_version, schema_revision }
reviewkit health [--cwd]                                 -> { tools: { git, gh, herdr }, gh_auth, state_dir }
reviewkit schema                                         -> JSON Schema（全型）

reviewkit session open --pr <number|url> [--cwd]
                                                         -> session   gh でメタ取得、bare mirror + worktree、targets、threads
                                                            番号は --cwd の origin のリポジトリ。URL ならそのリポジトリ（origin と違ってもよい）
reviewkit session open --local [--base REV] [--head REV|worktree] [--cwd]
                                                         -> session   既存があれば再開して refresh。--base A... は A と head の
                                                                      merge-base（refresh ごとに求め直す）。A...B で --head を省くと head = B。
                                                                      A..B は base = A、head = B（空の側は HEAD）。両側を書いた base と --head の併用は E_INVALID
reviewkit session open --session ID                      -> session   再開
reviewkit session get     --session ID                   -> session
reviewkit session list    [--cwd]                        -> data: SessionSummary[]
reviewkit session refresh --session ID [--path P ...]    -> session   Source 再実行。base / head が動いたら再アンカー。PR は pr.json も取り直す
                                                                     --path は将来の部分更新用で、現状は受け取るだけで全体を再計算する
reviewkit session close   --session ID [--purge]         -> data: { closed: true }   purge で worktree と state を消す

reviewkit target list --session ID                       -> data: Target[]
reviewkit target viewed --session ID --path P [--undo]   -> session   既読の付け外し。head_blob を一緒に記録する
reviewkit pr show --session ID                           -> data: PrInfo   pr.json から。無ければ E_NO_SESSION ではなく data: null
reviewkit file base   --session ID --path P              -> data: { exists, binary, too_large, lines: string[]|null, raw? }
                                                                     raw: UTF-8 でないときの生のバイト（base64）。lines は置換文字入りの代替
                                                                     targets 外の path も可。base で未追跡なら exists = false
reviewkit file head   --session ID --path P              -> 同じ形。head がコミットのセッションだけ（作業ツリーなら E_INVALID）。
                                                            view はこれを読み取り専用で head 側に出す（作業ツリーの別ブランチや未コミットの
                                                            変更で、表示とコメントの行がずれないため）。文字コードは fileencodings の順に試す

reviewkit comment add    --session ID  (stdin: CommentInput)         -> session
reviewkit comment update --session ID --id CID (stdin: CommentPatch) -> session
reviewkit comment remove --session ID --id CID                       -> session
reviewkit comment list   --session ID [--path P]                     -> data: Comment[]

reviewkit decision set   --session ID (stdin: DecisionInput)  -> session
reviewkit decision clear --session ID                    -> session

reviewkit sink list                                      -> data: SinkInfo[]   { name, available, reason, runtime: "cli" }
reviewkit sink targets --sink herdr [--cwd]              -> data: SinkTarget[]   picker 用
reviewkit submit --session ID --sink github|herdr [--dry-run] [--options -]
                                                         -> data: SinkResult（dry-run は data.preview: SubmitPreview）, session
                                                            --options - のときだけ stdin から SubmitOptions を読む
reviewkit submit apply --session ID [--consume] (stdin: SinkResult の sent / failed / skipped / thread_actions / decision、
                                                            省略可の url / mode / attempted / decision_version)
                                                         -> session   Lua 側 sink の結果を core に適用。at は CLI が付ける。ロック待ちは記録待ち（送信箱）
reviewkit submit apply --session ID --drain              -> session   記録待ちの結果を記録するだけ（stdin を読まない）

reviewkit history list    --session ID                   -> data: Comment[]   キューを出たコメント（新しい順）。200 件を超えると消費済みから捨て、消しただけのものは 1000 件を超えた分を古い順に捨てる
reviewkit history restore --session ID --id CID          -> session   キューの末尾へ戻す。deliveries は残す（同じ sink へは再送しない）。既にキューにあれば何もしない。
                                                            セッションの宛先（PR は github、local は herdr。Lua の sink も宛先とみなす）で消費されたもの
                                                            （Delivery.consumed）は E_INVALID（編集すると二重に届くため）。宛先でない sink に
                                                            `consume` で渡したもの（PR のコメントをエージェントへ）は戻せる
                                                            consumed を記録する前の版（revision 7 未満）が書いた history は、送信済みの配送をすべて消費済みとして読む
reviewkit history clear   --session ID                   -> session   data: { removed }
reviewkit comment clear   --session ID --delivered [--sink NAME] -> session   data: { moved: CID[] }
                                                            mode = sent の配送があるコメントを history へ移す。--delivered は必須（全消しの事故を防ぐ）

reviewkit thread list    --session ID [--refresh]        -> data: Thread[]   --refresh は session 全体を取り直し、session も返す
reviewkit thread resolve --session ID --thread TID [--undo] -> session       GitHub 上の状態と同じになる操作は積まずに消す
```

- `--dry-run` は sink によらない `SubmitPreview` を返す。コメントは id で並べ（行に置くもの / 本文に書くもの / skipped）、
  返信・スレッド操作・Decision の event、まとめて送る本文（review の body / herdr のプロンプト）と送り先を添える。
  path や行はクライアントが自分の Session から引く。sink が実際に送るリクエスト（GraphQL / herdr の引数）は
  `raw` に入るが契約外で、sink の実装に合わせて予告なく変わる
- `SubmitPreview.digest`（github だけ）は送る内容（review の event・本文・行コメント・返信・スレッド操作）のハッシュ。試行ごとの
  marker は含めない。プレビューも中断した送信の照合を（保存せずに）行ってから作るので、確認した内容と実際に送る内容が揃う。
  submit は `expect_digest` を、ロックを取って照合を済ませた後の送る直前の内容と比べる。確認から送信までの間に別のインスタンスが
  decision やコメントを変えた・refresh した場合は送らず、Lua はプレビューを出し直す。`--expect-rev` は使わない（既読や
  送る内容に関係の無い変更でも弾くため）。照合だけで送るものが無くなったときのプレビューは空で、確認すると submit が照合を保存する
  （その間に別のインスタンスが保存していれば ok で `already recorded`）。照合を保存した後で送信が断られたときは、
  エラーの `details.recovered` に照合で送信済みと分かったコメントの id が入る
- `SinkTarget { id, name?, status?, cwd?, title? }` は送り先の候補。`id` を `HerdrOptions.target` に渡す。
  `sink targets` は agent の無い pane も含めて全 pane を返す（stage は任意の pane に送れる）。
  `E_AMBIGUOUS` の `details.candidates` は agent が動いている pane だけ
- `submit` は送信前の失敗（未認証、pane 未解決）だけ `ok: false`。リクエストを送った後は部分失敗でも `ok: true` と SinkResult

## 5. 状態の所有と並行性

- 保存先は `<state-dir>/sessions/<id>/session.json`。PR の付帯情報（conversation、checks）は同じディレクトリの `pr.json` に分け、
  Session の封筒を重くしない。変更系は advisory lock → 読む → 適用 → `rev += 1` → temp + rename。
  ロックはプロセス終了で解放される。CLI のロック待ちは短く（2 秒）、Lua の `cli.call` が `E_LOCKED` を間隔を空けて数回再試行する。
  PR の refresh（gh と git fetch）はロックの外で取得し、ロックの中では読み直したセッションに当てるだけにする。
  送信はロックを持ったまま行う（送信中の編集を失わないため。版の照合で外せるようになるまで）。持つのは github の送信
  （中断した送信の照合で gh から PR を取り直す段も含む。gh の呼び出しごとに timeout があり、それが送信の間続く）と herdr の送信（`agent prompt` / `pane send-text`）。
  その間の変更系（コメントの追加・編集、decision）は再試行の後に `E_LOCKED`（hint で送信中の可能性を示す）で失敗する。
  入力窓から保存したものが失敗したときは、Lua が入力した本文のまま入力窓を開き直し、本文を失わない。
  PR の取得はセッションごとの fetch.lock で直列にし、リポジトリごとの mirror は `mirror.lock` で守る（作成は一時ディレクトリから rename）。
  git の fetch / clone は `REVIEWKIT_GIT_TIMEOUT_MS`（既定 120 秒）で打ち切り、認証のプロンプトは出さない。
  git は呼び出し元の環境に左右されないよう、リポジトリを指す変数（`GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE` など。
  フックやコミットエディタから起動した Neovim が持つ）と pathspec の解釈を変える変数を外し、`GIT_OPTIONAL_LOCKS=0` で
  `status` が index.lock を取らないようにし（利用者の `git add` / `commit` と衝突しない）、制御端末を持たない新しいセッションで
  動かす（ssh のホスト鍵やパスフレーズの確認が端末に出て、ロックを持ったまま待つことがない。確認が要る接続は失敗する）。
  askpass（`GIT_ASKPASS`、`SSH_ASKPASS`、`core.askPass`）も使わず、`DISPLAY` も外す（エディタやデスクトップの入力窓が出て
  同じく待つため。古い OpenSSH は `SSH_ASKPASS_REQUIRE` を知らず、DISPLAY があれば既定の askpass を出す）。
  timeout で打ち切るときはセッションのプロセスグループごと止める
- PR の実体化は `<state-dir>/repos/<key>/mirror.git`（bare。ローカルからの clone でオブジェクトはハードリンク）と
  `<state-dir>/repos/<key>/pr/<n>/`（worktree）。2 回目以降は同じ worktree を使い回し、head が同じなら fetch しない。
  利用者の checkout から mirror を作るのは、同じリポジトリで、浅い clone でも部分 clone でもないときだけ（欠けた履歴や blob を
  mirror に持ち込むと merge-base や読み出しが失敗する）。作るのに失敗したら空の mirror から fetch する。
  浅い・部分 clone から作られた既存の mirror（`shallow` や `*.promisor` がある）は作り直す。
  base は open / closed を問わず GitHub が記録した base の commit を id で取る（マージ後や retarget 前に base ブランチが
  消されていることがある）。リモートにその commit や参照が無いと git が答えたときだけ（force-push で消えた）ブランチで取り直す
  （ネットワークの失敗で取り直しても同じく失敗するため）。
  ユーザーのリポジトリには何も書かない
- 複数 Neovim で同じ Session を開いたとき、Lua は常に返ってきた Session でキャッシュを置き換える
  （コメント単位の last-writer-wins）。他インスタンスの変更は `state_path` の `vim.uv.new_fs_event`
  （150 ms デバウンス）→ `session get` → etags 差分でイベント発火。`--expect-rev N` で `E_CONFLICT` を返す
  CAS は定義だけして最初は使わない
- 選択送信: sink には選んだコメントだけを持ち decision とスレッド操作を持たないセッションの写しを渡し、結果は本物のセッションに
  適用する。Lua の sink にも同じ写しを渡し、`attempted` は選んだ id にする。apply は `attempted` 外の `sent` を無視する
- 二重送信の防止: gh を呼ぶ前に `pending_submit.json` を書く。次の GitHub への submit で残っていれば threads を取り直し、
  照合してから消す（消すのは結果を保存した後、ロックを持ったまま。外で消すと次の送信が書いた記録を消しうる）。
  送信がロックを取った時点で記録が残っていれば（照合を見た後に別の送信が中断した）、書き換えずに再試行可の E_CONFLICT で止める。行コメントと返信の本文の末尾には、独立した行で `<!-- reviewkit:<hash> -->` を付ける。hash は
  （コメントの id, 試行の時刻 `at`）から作るので、同じコメントの前の試行の投稿とも、Web UI で書いた同じ本文とも区別できる。
  照合はこの marker の一致で行い、viewer が分かっていれば投稿者が viewer であることも求める（引用で marker を写した他人の投稿を拾わない）。
  review は、運んだ行コメントのどれかが見つかれば投稿済み（review と行コメントは 1 つの mutation で作られる）。
  diff 外のコメントを書き込んだ review は本文にも marker を付け、それで探す。decision だけの review は marker を持たず、
  「自分の、同じ state・同じ本文の、試行の開始以後の review」で探す。marker を付ける前の版が書いた pending（`marked` が無い）は
  本文と時刻で照合する。本文で照合する場合（decision だけの review と、古い pending）は、viewer が取れなければ照合せず、
  再試行可のエラーで止める（誰の投稿でも一致してしまうため）。
  照合はキューだけでなく履歴（中断の後に別の sink が消費したもの）も探し、見つかったコメントに GitHub への配送を記録する
  （記録しないと履歴から戻して二重に投稿できてしまう）。
  marker は表示（threads、`pr.json`）に出す前に剥がし（本文の最後の行だけ。途中の同じ形の行は他人の本文）、照合は剥がす前の本文で行う。
  残っている間は Session の `pending_submit` が true（ファイルの有無から毎回求める）で、Lua は開いたときに知らせる。
  記録には review が decision を運んだかも残し、運んでいない review（comment だけの送信）を照合しても decision は消さない
- 送信の故障時の挙動（送信の経路を変えるときはここを先に直す）:

  | 経路 | 途中で落ちる | 2 回実行 | 相手が応答しない |
  |---|---|---|---|
  | github | pending が残り、次の github submit が照合してから送る。照合が済むまで、pending に載ったコメントの編集・削除と、decision を運んだ pending があるときの decision の変更は E_CONFLICT（照合が送ったときの本文で探すため、変えると送信済みを見失い二重に投稿する）。`comment clear` も pending に載ったコメントはキューに残す（照合はキューの中を探す） | 照合で送信済みと分かったものは送らない。GitHub へ sent 済みは `delivered` で skip | `outcome = unknown`、再試行不可。pending を次回照合。どのフィールドの失敗かを示さないエラー（タイムアウト等）や data の無い応答も同じ扱い |
  | herdr | 送信後・保存前に落ちると記録が無く再送しうる（エージェント宛てで公開されない） | 自分へ sent 済みのコメントと decision は送らない | 全体失敗、`outcome = unknown`。印は試行分だけ |
  | 選択送信（comment_ids） | github は選んだ分だけの COMMENT review を送り、pending には decision を運ばないと記録する（照合しても decision は消さない） | 送った分は消費されてキューに無く、その id は落とされる。全部落ちれば nothing to submit | 経路ごとの規則と同じ |
  | Lua sink（submit apply） | 送信後・apply の CLI が結果をファイルに書く前に落ちると記録されず再送しうる（sink の責任）。書いた後なら次の書き込みが記録する | 消費済みはキューに無い。`version` / `decision_version` が違うものは記録しない。Lua の sink で送る前に Lua はディスク上の Session（`session get`）を読み、記録待ちがあれば記録を試み、残れば E_CONFLICT で止める（別インスタンスの記録待ちも見える。記録が `E_LOCKED` 以外で失敗したときはそのエラーを返す） | sink の責任 |
- Lua の sink の結果（送信箱）: `submit apply` は受け取った結果に CLI の時刻を `at` として付け、ロックを取る前に
  `<session>/applies/<ulid>.json`（`{ revision, consume, result }`）へ書く。ロックが取れればその場で記録して `ok`。書いた後に
  記録できなければ（他の送信がロックを持っている、新しい版が書いたセッションなど）、記録待ちのまま `ok`（`summary` に理由と
  記録待ちと書き、`session.pending_apply` が true）を返す。エラーにすると、配送済みのものをクライアントがもう一度送るため。
  記録待ちは、そのセッションへの次の書き込み（どのコマンドでも）が自分の変更の前に古い順で適用し、保存の後にファイルを消す
  （保存と削除の間で落ちても、同じ `(sink, at, mode)` の配送は二度記録しない）。Lua は `pending_apply` を見ると
  `submit apply --drain` で記録を促す（ロックを持つ送信が保存すると監視で気づく。`E_LOCKED` で失敗したときだけ間隔を空けて
  促し直す。他のエラーは待っても直らないので、Session が次に変わるまで促さない）。
  プレビュー（`--dry-run`）は記録待ちを適用した写しから作る（送信は書き込みの冒頭で適用するので、そうしないと digest が合わない）。新しい版が書いたファイルは読まずに残す（`pending_apply` には数える。別インスタンスの配送なので、
  数えないと Lua の sink が同じコメントをもう一度送る）。
  壊れたファイルは `applies/rejected/` に移す（他の書き込みを失敗させない）
- gh には github.com を含め常に `repo.host` を渡す（`--hostname`、`--repo host/owner/name`）。渡さないと gh は `GH_HOST` に従い、
  GitHub Enterprise を既定にしている利用者の github.com の PR が別のホストに読み書きされる

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
  log.lua             CLI の呼び出しとエラーの直近の記録（:ReviewLog）
  maps.lua            既定キーマップの表（mode、lhs、action、desc、diff ビューのバッファだけか）
  install.lua         ensure()（Releases から取得して検証、合わなければ cargo build → <plugin>/bin/）
  view/diff.lua       タブ・左右ウィンドウ・次 / 前の移動。base 側は file base で取得した読み取り専用バッファ。
                      左右の位置合わせと色付けは Neovim の diff モード（diffthis）。hunk 間の移動とコメント可否は Target から
  view/annotate.lua   extmark の注釈（stale は薄く）
  view/queue.lua      一覧（quickfix）
  view/input.lua      入力窓
  view/thread.lua     カーソル行のスレッドを float で表示。r で返信（reply_to 付きコメント）、R で resolve の切り替え
  view/preview.lua    送信前のプレビュー（SubmitPreview）を見せて確認する
  orchestrator.lua    :Review の配線、worktree の監視、既定キーマップ
  sink/clipboard.lua  Lua 側 sink の最小例
plugin/reviewkit.lua  コマンドと maps.lua の <Plug>(reviewkit-*) の定義だけ（orchestrator は読み込まない）
scripts/gen-types.lua `reviewkit schema` の JSON Schema から types.lua を生成
```

### core の公開 API

CLI を呼ぶものは非同期 `cb(err, result)`、キャッシュ読みは同期。返るテーブルは読み取り専用で、書き換えてはならない。

```
core.open(spec, cb)          spec = { kind = "pr", pr = 123 | url, cwd? } | { kind = "local", base = "HEAD~3", head?, cwd? }
core.resume(id, cb)
core.refresh(opts, cb)
core.close(opts, cb)
core.session() / core.targets() / core.comments() / core.decision() / core.threads()
core.target(path)            -- path の Target（無ければ nil）
core.relpath(name)           -- バッファ名を session の path に（worktree の外なら nil）。両側を realpath で解決する
core.is_commentable(path, side, line)      -- Target.commentable から
core.add_comment(input, cb) / core.update_comment(id, patch, cb) / core.remove_comment(id, cb)
core.set_decision(decision, cb) / core.clear_decision(cb)
core.set_viewed(path, on, cb)
core.resolve_thread(id, resolved, cb) / core.refresh_threads(cb) / core.pending_thread_action(id)
core.pr_info(cb)               -- PrInfo。表示は持たないが取り出せる
core.side_file(side, path, cb) -- file base / file head の data。core.base_file(path, cb) は side = "base"
core.submit(sink, opts, cb)  -- Rust 側 sink は submit。Lua 登録 sink は impl.submit(session, opts, cb) の結果を submit apply に渡す
                             -- opts は setup の sinks.<name> に重ねる。cb(err, result, session)
core.preview(sink, opts, cb) -- submit --dry-run。cb(err, SubmitPreview)。返った Session でキャッシュも更新する
core.register_sink(name, impl)  -- impl = { submit, available?, consumes? }。available() は boolean, reason? を返す。
                             -- consumes(session, requested) は送れたコメントをキューから消すか（既定 false）
core.sinks(cb)               -- sink list と登録済みの Lua sink（runtime = "lua"）をマージ。同名は Lua を優先
core.sink_names()            -- 最後の sinks() と登録済みの名前。:ReviewSubmit の補完用（同期）
core.history(cb) / core.restore(id, cb) / core.clear_delivered(sink?, cb) / core.clear_history(cb)
core.on(event, fn, group?)   -- User autocmd の薄い包み。autocmd の id を返す
```

### イベント

`User` autocmd。`data = { session_id, rev }`（`ReviewSubmitted` は `result` に SinkResult も載せる）。etags の差分で発火するので、1 応答で複数発火することも、無変更なら発火しないこともある。

`ReviewSessionOpened`、`ReviewTargetsChanged`、`ReviewQueueChanged`、`ReviewDecisionChanged`、`ReviewThreadsChanged`
（threads と thread_actions の両方）、`ReviewViewedChanged`、`ReviewSubmitted`、`ReviewSessionClosed`

### 設定とコマンド

```lua
require("reviewkit").setup({
  cli = { path = nil, dev = false, state_dir = nil },   -- state_dir は CLI の --state-dir として渡す
  tools = { git = nil, gh = nil, herdr = nil },   -- REVIEWKIT_* として CLI に渡す
  set_keymaps = true,
  sinks = {
    herdr = { agent_type = "claude", header = nil, match_foreground_cwd = true },   -- HerdrOptions として渡す
    -- どの sink の表にも consume = true | false を書ける（SubmitOptions.consume。既定は sink が決める。
    -- github は二重投稿を避けるため常に消費し、clipboard の写しは消費しない）
    github = { confirm = true },                    -- :ReviewSubmit github は送る内容を見せて確認する
    clipboard = { register = "+" },
  },
})
```

`:Review pr <number|url>`、`:Review diff <base> [<head>]`、`:Review resume [id]`、`:Review refresh`、`:Review threads [refresh]`、
`:ReviewSubmit herdr|github|clipboard`（`:'<,'>ReviewSubmit` はキューの一覧で選んだ行だけ）、`:ReviewClose[!]`、`:ReviewHistory`、`:ReviewInstall`、`:ReviewLog`。
quickfix の一覧では、履歴の `r` でキューへ戻し、キューの `D` で配送済みを履歴へ移す（確認あり）。キューの一覧で行を選んで
`<leader>rs`（visual）か `:'<,'>ReviewSubmit {sink}` を使うと、その行のコメントだけを送る（`comment_ids`）。`:ReviewClose` は未送信の
コメントがあると確認する（`!` の purge ではコメントも消えると明示する）。diff ビューのバッファだけのキーは
`]f` `[f`（ファイル）、`]h` `[h`（hunk）、`]t` `[t`（行のあるスレッド。outdated とファイル単位はスレッド一覧から）、
`]u`（未読の次のファイル）。`setup()` を呼ばなくてもコマンドは使える。

## 8. 未決事項

- `anchor.context` で ±N 行を探索して `stale` を自動で解くか
- GitHub の suggestion ブロックの送信形式
- `--expect-rev` を view のどの操作で使うか
- 未保存バッファの内容を stdin で渡す `refresh --content -`
- source-graph は同じ CLI のサブコマンドか、別バイナリ `reviewkit-graph` か
