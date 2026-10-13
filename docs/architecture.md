# reviewkit.nvim — アーキテクチャ設計（粗粒度）

Neovim 上でコードレビューを完結させるための、小さな部品の集合体。
「レビュー対象を拾う」「コメントを溜める」「送る」を分離し、各部品を個別に差し替え・進化させる。

対象は 2 種類のレビューで、手順は同じ。

- 他人の PR のレビュー（送り先: GitHub の review）
- AI エージェントが書いたコードのレビュー（送り先: herdr 上のエージェントへのプロンプト）

## 全体像

```
   Source                       Core                          Sink
 (対象を拾う)             (状態を持つ・配る)                   (送る)

 ┌──────────┐        ┌──────────────────────┐        ┌──────────────┐
 │ local     │──┐     │ Session               │   ┌───▶│ herdr        │
 ├──────────┤  │     │  ├ targets  (Target[]) │   │    ├──────────────┤
 │ pr        │──┼────▶│  ├ queue    (Comment[])│───┼───▶│ github review│
 ├──────────┤  │     │  └ decision  (Decision)  │   │    ├──────────────┤
 │ (graph)   │──┘     │ events: User autocmd  │   └───▶│ (clipboard…) │
 └──────────┘        └──────────┬───────────┘        └──────────────┘
                                 │ 購読
                      ┌──────────▼───────────┐
                      │ View                  │
                      │  ├ diff layout        │  対象を読む・コメントを打つ
                      │  ├ annotations        │  行の印・バッジ
                      │  └ queue list / input │  一覧・入力窓
                      └──────────────────────┘
                                 ▲
                      ┌──────────┴───────────┐
                      │ Orchestrator (preset) │  :Review pr 123 / :Review diff HEAD~3
                      └──────────────────────┘
```

Core が中心にあり、Source は Core に Target を**流し込む**、Sink は Core から Comment と Decision を**読み出す**。
View は Core のイベントを購読して描くだけ。Source と Sink は互いを知らず、View も知らない。

## 部品一覧

| 部品 | 責務 | 依存 | 置き換え・拡張の例 |
|---|---|---|---|
| **core** | Session の生成・破棄。Target / Comment / Decision の保持。変更イベントの発火。Source / Sink の登録口 | なし（純 Lua） | ― |
| **source-local** | 作業ツリーや任意の 2 リビジョン間の差分から Target を作る | lib/git | base の指定（HEAD、merge-base、任意 ref） |
| **source-pr** | PR 番号 / URL から base と head を取得し、ファイルを実体化して Target を作る。既存スレッドも Target の付帯情報として取り込む。差分の計算は lib/git に寄せる | lib/gh、lib/git | GitLab / 他ホスティング |
| **source-graph**（将来） | 変更が影響する呼び出し元・参照元を解析し、diff 外の箇所を Target に追加する | LSP / treesitter | 依存グラフの深さ・方向の設定 |
| **view-diff** | Target を side-by-side で表示し、対象間の移動（次のファイル / hunk）を提供する。head 側は実ファイル、base 側は追従する読み取り専用バッファ | core | inline 表示、既存 diff プラグインへの委譲 |
| **view-queue** | コメント入力窓、キュー一覧、行注釈。キューの編集・削除 | core | 一覧の表示形式（float / panel / quickfix） |
| **sink-herdr** | queue を 1 プロンプトに整形し、cwd から解決した herdr pane へ送る | lib/herdr | agent 種別ごとの参照形式、tmux 等の別 transport |
| **sink-github** | queue と Decision を 1 リクエストで PR の review として送る。返信・resolve も扱う | lib/gh | GitLab |
| **orchestrator** | 上記を束ねる preset。`:Review pr 123` のような 1 コマンドで Source → View → Sink を配線する | 全部 | 個人の好みのキーマップ・既定値 |

core / view が内側、source / sink が外側。外側の部品は core の公開 API だけを使い、view を直接触らない。

外部コマンドの呼び出しは **lib** 層に集約し、Source / Sink はそれを使う。lib は Session を知らない。

| lib | 中身 | 使う部品 |
|---|---|---|
| lib/git | 差分抽出、merge-base、`git show`、PR の実体化（private clone） | source-local、source-pr |
| lib/gh | PR のメタデータ・スレッド取得、GraphQL の実行 | source-pr、sink-github |
| lib/herdr | pane の解決、プロンプト送信 | sink-herdr |

source-local と source-pr の違いは「base / head をどう決めるか」と「ファイルをどこに実体化するか」だけで、
差分の計算は両者とも lib/git に寄せる。

## データの流れ

1. Orchestrator が Source を選び、Session を開始する
2. Source が Target の集合を core に登録する（`session.targets`）
3. View が Target を受けてレイアウトを開き、ユーザーが読み進める
4. ユーザーが行や範囲にコメントを打つ。View は Comment を作って core の queue に積む
5. core が変更イベントを発火し、注釈や一覧が再描画される
6. ユーザーが Decision（approve / request_changes / comment と総評）を選ぶ。PR 以外では省略できる
7. Orchestrator が Sink を呼ぶ。Sink は queue と Decision を読み、送信する
8. Sink は「送れたもの」「送れなかったもの」を返す。core は送れたものをクリアし、残りは queue に留める

## 部品間の契約（概念）

型定義は次フェーズで確定する。ここでは各データが**何を持たなければならないか**だけ決める。

- **Target**: どのファイルのどの範囲を読むべきか。`path`、範囲、比較元 `base_ref` と比較先 `head_ref`、拾った理由（変更 / 影響範囲 / 既存スレッド）。
  複数 Source からの Target は同じ Session に混ざる
- **Comment**: どこに何を言ったか。`path`、`side`（base / head）、行と開始行、本文、種類（指摘 / 提案 / 質問 など）、対象の ref。
  `side` と ref は GitHub 送信に必須なので、AI 向けでも必ず持たせる
- **Decision**: Session に 1 つ。`approve` / `request_changes` / `comment` と総評本文。queue の要素ではない。
  値は GitHub の review `event` に合わせ、sink-github がそのまま詰め替える
- **Session**: 上記 3 つと、Source の識別（repo、PR 番号、base / head の commit）を束ねる単位。
  PR ごとに永続化し、Neovim を閉じても再開できる

連携の仕組みは 2 つに限定する。

- **イベント**: `User` autocmd。`ReviewTargetsChanged` / `ReviewQueueChanged` / `ReviewDecisionChanged` / `ReviewSessionClosed`。View と Sink はこれだけを購読する
- **登録**: `core.register_source(name, impl)` / `core.register_sink(name, impl)`。Orchestrator は名前で選ぶ

## 既存資産の扱い

- **herdr-send-queue.nvim**: `core/herdr.lua`（送信 transport）と `review/format.lua`（プロンプト整形）が sink-herdr の中身になる。
  queue と view は新 core / view-queue に吸収する。移行が終わるまでは独立プラグインとして並存させる
- **agent-review.nvim**（MIT）: `pr/` 配下（gh.lua、mirror.lua、review.lua、threads.lua）が source-pr と sink-github の参考実装。
  `session.lua` が view-diff の参考実装。書き起こしではなく移植で始める

## 配布単位

**最初は 1 リポジトリ**に全部品を置き、`lua/reviewkit/{core,lib,source,view,sink}/` のディレクトリ境界で分ける。
部品間の呼び出しは `require("reviewkit.core")` のような公開モジュール経由に限り、内部モジュールを越境して呼ばない。

別リポジトリへ切り出す条件は「その部品に外部の利用者が付いた」か「依存（gh、herdr など）の有無で導入可否が分かれる」の
どちらか。source-graph のように重い依存を持つものは、最初から別リポジトリの候補。切り出す際は `reviewkit-<name>.nvim` の接頭辞で揃える。

## 主要な設計判断

- **D1. コメントは GitHub の pending review ではなくローカルに溜め、submit 時に 1 リクエストで送る。**
  AI 向け送信と同じ queue を使えるようにするため。GitHub 側の pending review は扱わない
- **D2. head 側は実ファイルとして開く。** LSP クライアント（組み込み LSP、coc.nvim 等）の定義ジャンプなど、エディタの機能をそのまま使うため。
  PR の場合は作業ツリーを汚さず、state ディレクトリ配下の private clone に実体化する
- **D3. Decision は queue の要素ではなく Session の属性。** PR 全体に 1 つしか無く、寿命も送り先もコメントと違うため
- **D4. Sink は部分的な失敗を返す。** GitHub は差分行と前後 3 行にしかコメントを置けない。
  diff 外へのコメントは AI には送れて GitHub には送れない。core はこの差を隠さず、Sink の結果で queue を更新する
- **D5. 連携手段はイベントと登録の 2 つだけ。** 部品が増えても結合点を増やさないため
- **D6. AI との対話は範囲外。** 対話は herdr pane 内のエージェントと直接行う。本ワークフローは「送る」までを担う

## 未決事項

- Comment の「種類」の集合（指摘 / 提案 / 質問 / 称賛）と、GitHub の suggestion ブロックへの対応
- PR の head が進んだときの Comment の追従（行ずれの扱い）
- Session の永続化形式と置き場所（state ディレクトリ配下、PR 単位）
- view-diff を自前で持つか、codediff.nvim 等に委譲する口を設けるか

## 次のステップ

1. Target / Comment / Decision / Session の型定義と、core の公開 API を `docs/design.md` に書く
2. source-local と sink-herdr で縦に 1 本通す（herdr-send-queue からの移植）
3. source-pr と sink-github を agent-review.nvim から移植する
