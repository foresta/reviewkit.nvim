# reviewkit.nvim — 技術選定

`docs/architecture.md` の部品を何で作り、どう検証し、どう配るかを決める。
型と CLI の契約そのものは `docs/design.md` に書く。

## 言語の分担

| 言語 | 担当 | 理由 |
|---|---|---|
| **Rust**（CLI `reviewkit`） | core（Session の状態・永続化）、source、sink、lib | ロジックの正しさを型と `cargo test` で守りたい。Neovim 以外からも使える |
| **Lua** | view、orchestrator、core shim | Neovim の API 操作は Lua でしか書けない |

境界は **argv + stdin JSON → stdout JSON** の 1 回実行コマンド。cdylib（nvim-oxi 等）は採らない。
Neovim の ABI と LuaJIT に結合せず、配布が `cargo install` かバイナリ 1 つで済むため。

各サブコマンドは lib 内の `fn run(ctx, <clap のサブコマンド型>) -> Result<Response>` で、入力の JSON は各コマンドが
stdin から自分で読む。将来 stdio で JSON 行を受ける `serve` を足すときは、引数と入力を値で受け取る形に
コマンドの入口を作り直す必要がある（stdin は要求の流れに使われるため）。JSON の型は変えずに済む見込みだが、
要求の封筒（どのコマンドを呼ぶか、要求と応答の対応付け）は新しく契約に足す。

## Rust

- stable、edition 2024。パッケージは 1 つ（`cli/`、`[lib]` + `[[bin]] reviewkit`）。lib を別クレートに分けるのは外部利用者が付いてから
- クレート

  | 用途 | クレート | 補足 |
  |---|---|---|
  | サブコマンド | `clap`（derive） | |
  | 契約 | `serde` / `serde_json` / `schemars` | `schemars` で JSON Schema を出す。LuaCATS への変換は Lua 側（Rust は Lua を知らない） |
  | エラー | `thiserror` | 全エラーに安定した `code` を持たせるため `anyhow` は使わない |
  | 外部コマンド | `std::process::Command` / `which` | 非同期ランタイムは入れない。呼ぶのは git / gh / herdr だけ |
  | 永続化 | `fd-lock` / `tempfile` | advisory lock と temp + rename の原子的書き込み |
  | id / 時刻 / ハッシュ | `ulid` / `jiff` / `sha2` | Comment の id、RFC 3339、etags |
  | ログ | `log` / `env_logger` | stderr に出す。stdout は JSON 専用 |
  | テスト | `assert_cmd` / `insta` / `tempfile` | 封筒の JSON スナップショット |

- **diff は git に任せる**。`git diff -U3 -M -l1000 --no-color --no-ext-diff --diff-algorithm=myers --indent-heuristic` を Session 単位で 1 回実行し、出力をパースする（アルゴリズムと rename 検出の上限は利用者の設定に左右されないよう固定する。hunk の境界や rename の判定が GitHub とずれるため。上限は git の既定値。rename 検出はファイル数の 2 乗でかかり、セッションのロックの中で走るので無制限にはしない）。
  `@@` ヘッダから GitHub のコメント可能範囲（変更行 ±3 行）が直接得られ、hunk 内の `-` / `+` の連なりから
  0 コンテキストの `Hunk[]` を作る。rename と binary の判定も git の出力に従う。untracked は `git ls-files --others --exclude-standard` で拾う。diff クレートは入れない
- git / gh / herdr は trait（`Git`、`Gh`、`Herdr`）越しに呼ぶ。core の単体テストはメモリ上の fake、
  結合テストは実 `git` + fake `gh` / `herdr`（`cli/tests/fixtures/bin/` のシェルスクリプトを PATH の先頭に置く）

## Lua

- 全モジュールに LuaCATS 注釈を付ける。契約の型 `lua/reviewkit/types.lua` は手書きせず、
  `reviewkit schema`（JSON Schema）を `nvim -l scripts/gen-types.lua` で変換した出力をコミットする。CI でずれを検出する
- 型チェックは `lua-language-server --check lua/`（`.luarc.json` で Neovim のランタイム定義を指定）。整形は `stylua`
- `vim.diff` は使わない。hunk は CLI が Target に載せて返し、view は再計算しない。
  左右の表示の位置合わせだけは Neovim の diff モード（`diffthis`）に任せる
- Neovim >= 0.12。`vim.system`、`vim.uv`、`vim.pack` を前提にする

## テスト

| 段 | コマンド | 内容 |
|---|---|---|
| Rust 単体 | `cargo test`（lib） | diff パーサ、コメント可能範囲、行ずれ、SinkResult の適用、GraphQL の組み立て |
| Rust 結合 | `cargo test`（`tests/`） | `assert_cmd` で CLI を起動。一時 git リポジトリ + fake `gh` / `herdr`。封筒を `insta` でスナップショット |
| Lua stub | `make test-lua` | mini.test（headless）。`tests/fixtures/bin/reviewkit` は `nvim -l` の Lua スクリプトで固定の封筒を返す。shim のイベント差分・エラー変換と view を検証 |
| Lua 結合 | `make test-integration` | `cargo build` した debug バイナリを `REVIEWKIT_BIN` で渡し、`:Review` からコメント、submit までを通す。`types.lua` のずれもここで検出 |

fake `gh` は引数のパターンと応答（`status` / `body` / `stderr` / `exit`）の対応表を持つスクリプトで、GitHub へは一切通信しない。

git はどの段でも開発者の global / system の設定（コミット署名、hooks）を読まない（Rust は `ISOLATED_GIT`、Lua は
`scripts/minimal_init.lua` が `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1` を設定する。Makefile を通さない実行と子 Neovim にも効く）。

CI（GitHub Actions、macOS + Linux）: `cargo test`、`cargo clippy -D warnings`、`cargo fmt --check`、
`lua-language-server --check`、`stylua --check`、`make test`。

## リポジトリ構成

```
reviewkit.nvim/
  Cargo.toml                workspace（members = ["cli"]）。root で cargo build が効く
  cli/
    Cargo.toml              package "reviewkit"、[lib] + [[bin]]
    src/main.rs             clap → commands::dispatch → 封筒 → 終了コード
    src/{cli,context,envelope,error,logging}.rs
    src/store/              state_dir、セッションの保存とロック、移行
    src/model/              session / target / comment / decision / hunk / thread / sink / pr_info / health
    src/core/               comments / commentable / anchor / apply / refresh / files / threads / viewed / etags / time（純ロジック）
    src/source/{local,pr,targets}.rs
    src/sink/{github,herdr}.rs
    src/ext/{git,gh,herdr,diffparse,tool}.rs   architecture.md の lib 層（`lib.rs` と衝突するので ext）
    src/commands/           サブコマンド 1 つにつき 1 ファイル
    tests/                  assert_cmd、fixtures/bin/{gh,git,herdr}、fixtures/diff（パーサの入力）
  lua/reviewkit/            Lua 側（design.md 参照）
  plugin/reviewkit.lua      コマンドと <Plug> マップの定義だけ。二重読み込みガード
  tests/                    mini.test、fixtures/bin/reviewkit（stub）
  scripts/                  minimal_init.lua、run_tests.lua、install.lua、gen-types.lua
  docs/
  Makefile                  deps / build / test-rust / test-lua / test-integration / test / check / fmt / gen-types / gen-schema / dist
  .github/workflows/        ci.yml、release.yml
```

## 配布と導入

- **バイナリ**: GitHub Releases に素のバイナリ `reviewkit-<target triple>`（macOS arm64 / x86_64、Linux x86_64 / arm64 は
  musl）と `SHA256SUMS` を置く。`.github/workflows/release.yml` がネイティブ runner の matrix で `make dist` し、タグ `vX.Y.Z`
  のときだけ公開する（PR ではビルドとスモークだけ）。リリースは draft で作り、全バイナリと `SHA256SUMS` を上げてから公開する
  （成果物の揃わない公開リリースを導入スクリプトが拾わないため）。公開に失敗した後の再実行は、残った draft に asset を上書きでアップロードしてから公開する。公開済みのリリースには触らない（公開中の asset を一時的に欠けさせないため）。cargo-dist は installer / manifest が不要なので使わない。
  `cargo install --git https://github.com/foresta/reviewkit.nvim reviewkit` も可
- **バージョンの規約**: `[workspace.package] version` はリリースの commit でだけ `X.Y.Z` にし、そこにタグを打ち、直後に次の
  `X.Y.Z-dev` に上げる。release.yml はタグと version の一致と、pre-release でないことを検査する
- **Lua 側の解決順**: `setup({ cli = { path } })` > `$REVIEWKIT_BIN` > `cli.dev = true` のとき
  `<plugin>/cli/target/{debug,release}/reviewkit` > `<plugin>/bin/reviewkit` > `exepath("reviewkit")`
- **ビルドフック**は `require("reviewkit.install").ensure()` に一本化する（`:ReviewInstall` は非同期版）
  - チェックアウトの `Cargo.toml` の version で取得するリリースを決める。安定版は `releases/download/vX.Y.Z`、`-dev` は
    cargo を優先し、無いときだけ `releases/latest` を警告付きで使う。`$REVIEWKIT_RELEASE_URL` で取得元を差し替えられる（テスト用）
  - curl で `SHA256SUMS` と asset を取り、sha256（`sha256sum` → `shasum` → `openssl`）を照合し、`version` を実行して
    schema_version（安定版は crate version も）が一致するものだけを `<plugin>/bin/.reviewkit.<pid>.<rand>` から rename で置く。
    実行中のバイナリを上書きしないため（macOS では SIGKILL になる）
  - 取得できなければ cargo でビルドする。できたバイナリの場所は `--message-format=json-render-diagnostics` の `compiler-artifact` から読む（利用者の `build.target` / `CARGO_BUILD_TARGET` で `target/<triple>/release` に出ても拾うため）。両方駄目なら、両方の理由と手で置く場所を返す
  - 既存の `bin/reviewkit` が目標に合っていれば何もしない（`-dev` は版が同じでも中身が古い可能性があるので、cargo があれば毎回ビルドし直す。cargo が無ければ、より古いはずの latest で置き換えずに残す）
  - lazy.nvim: `build = function() require("reviewkit.install").ensure() end`
  - dein: `build = "nvim --headless -l scripts/install.lua"`、`hook_add` で `setup()`
  - vim.pack: `PackChanged` autocmd で `kind ~= "delete"` のとき `ensure()`
- `:ReviewInstall` と `:checkhealth reviewkit`（解決したパス、`version`、`schema_version` の一致と `schema_revision`、git / gh / herdr の有無、state dir の書き込み可否）
- **契約バージョン**: CLI の `version` が `schema_version` と `schema_revision` を返す。Lua は最初の呼び出しで
  `types.lua` の `SCHEMA_VERSION` と一致し、`SCHEMA_REVISION` 以上であることを確かめ、満たさなければ止めて
  `:ReviewInstall` / checkhealth に誘導する（新しいプラグインが古い CLI に、知らない入力を送らないため）。
  同じ `schema_version` 内の変更は追加のみ
- **契約の互換検査**: 契約の JSON Schema を `docs/schema.json` にコミットし、`cargo test`（`cli/tests/schema.rs`）が
  現在の Schema と比べる。契約には入力と出力の両方があるので、既存のフィールドは型・必須かどうか・制約
  （format、上限下限、`additionalProperties`）を変えられない。同じ `schema_version` のまま、型・フィールド・enum 値・
  variant を消す、既存のフィールドを変える、必須のフィールドを足す、のどれかがあると落ちる。足すフィールドは
  `#[serde(default)]` か `Option` にする（古いクライアントは送らず、古い session.json には無い）。任意のフィールド・
  enum 値・variant の追加は通り、
  `make gen-schema` で `docs/schema.json` を更新する。比較の基準はブランチ上の `docs/schema.json` なので、その差分は
  PR のレビューで見る。Schema が変わったのに `schema_revision` が増えていなければ落ちる
- **セッションの移行**: `SCHEMA_VERSION` を上げるときは、`store::MIGRATIONS` に「旧版の session.json（JSON の値）を
  1 版進める」関数を 1 つ足す。読むときに古い版から順に適用するので、版を上げても未送信のコメントは残る。
  足し忘れはテスト（`every_schema_version_has_a_migration`）が落とす
- **書いた版の記録**: session.json のトップレベルに契約外のキー `_writer`（`revision`、`version`、`exe`）を書く。`SCHEMA_REVISION` は
  契約の Schema（入力も保存も含む）が変わるたびに +1 する（上の互換検査が上げ忘れを落とす）。
  自分より新しい revision が書いたセッションは、読めれば `session get` / `list` で返すが書き戻さない（`E_SCHEMA_VERSION`、
  `details.direction = "newer"`）。再開（`session open --session`）は refresh を書くので拒否される。`close --purge` は読めなくても消せる。
  pending_submit.json と pr.json は対象外（前者は送信の途中にだけ存在し、後者は refresh で作り直す）。
  旧版が知らないフィールドを消すのを防ぐため。足すフィールドは `#[serde(default)]` か `Option` にして、新版が古いファイルを読めるようにする
- Windows は非対応
