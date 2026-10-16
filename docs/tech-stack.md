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

各サブコマンドは lib 内の `fn run(ctx, Request) -> Result<Response>` で、将来 stdio で JSON 行を受ける
`serve` を足しても契約は変わらない。

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

- **diff は git に任せる**。`git diff -U3 -M --no-color --no-ext-diff` を Session 単位で 1 回実行し、出力をパースする。
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

CI（GitHub Actions、macOS + Linux）: `cargo test`、`cargo clippy -D warnings`、`cargo fmt --check`、
`lua-language-server --check`、`stylua --check`、`make test`。

## リポジトリ構成

```
reviewkit.nvim/
  Cargo.toml                workspace（members = ["cli"]）。root で cargo build が効く
  cli/
    Cargo.toml              package "reviewkit"、[lib] + [[bin]]
    src/main.rs             clap → commands::dispatch → 封筒 → 終了コード
    src/{cli,envelope,store,luacats}.rs
    src/model/              session / target / comment / decision / hunk / thread / sink_result
    src/core/               session / comments / decision / commentable / anchor / apply（純ロジック）
    src/source/{local,pr}.rs
    src/sink/{github,herdr}.rs
    src/ext/{git,gh,herdr,diffparse}.rs   architecture.md の lib 層（`lib.rs` と衝突するので ext）
    src/commands/           サブコマンド 1 つにつき 1 ファイル
    tests/                  assert_cmd、fixtures/bin/{gh,herdr}
  lua/reviewkit/            Lua 側（design.md 参照）
  plugin/reviewkit.lua      コマンド定義だけ。二重読み込みガード
  tests/                    mini.test、fixtures/bin/reviewkit（stub）
  scripts/                  minimal_init.lua、run_tests.lua、install.lua、gen-types.lua
  docs/
  Makefile                  deps / build / test-rust / test-lua / test-integration / test / check / fmt / gen-types
  .github/workflows/        ci.yml、release.yml
```

## 配布と導入

- **バイナリ**: `cargo install --git https://github.com/foresta/reviewkit.nvim reviewkit`、または GitHub Releases
  （`cargo-dist`。macOS arm64 / x86_64、Linux x86_64 / arm64 は musl）。タグ `v0.x.y` でプラグインと CLI を同時にリリースする
- **Lua 側の解決順**: `setup({ cli = { path } })` > `$REVIEWKIT_BIN` > `cli.dev = true` のとき
  `<plugin>/cli/target/{debug,release}/reviewkit` > `<plugin>/bin/reviewkit` > `exepath("reviewkit")`
- **ビルドフック**は `require("reviewkit.install").ensure()` に一本化する。最初は `cargo build --release` して
  `<plugin>/bin/` に置くだけ。Releases からのダウンロード（checksum 検証）は後で同じモジュールに足す
  - lazy.nvim: `build = function() require("reviewkit.install").ensure() end`
  - dein: `build = "nvim --headless -l scripts/install.lua"`、`hook_add` で `setup()`
  - vim.pack: `PackChanged` autocmd で `kind ~= "delete"` のとき `ensure()`
- `:ReviewInstall` と `:checkhealth reviewkit`（解決したパス、`version`、`schema_version` の一致、git / gh / herdr の有無、state dir の書き込み可否）
- **契約バージョン**: CLI の `version` が `schema_version` を返す。Lua は最初の呼び出しで `types.lua` の
  `SCHEMA_VERSION` と照合し、不一致なら止めて checkhealth に誘導する。同じ `schema_version` 内の変更は追加のみ
- Windows は非対応
