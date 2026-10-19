# debug-bin is phony on purpose: cargo, not make, knows when the sources
# changed, so a file target would keep testing a stale binary.
.PHONY: build debug-bin deps test test-rust test-lua test-integration check check-rust check-lua fmt gen-types

NVIM ?= nvim
DEBUG_BIN := target/debug/reviewkit

build:
	cargo build --release

deps: .tests/deps/mini.nvim

.tests/deps/mini.nvim:
	git clone --depth 1 https://github.com/echasnovski/mini.nvim $@

test: test-rust test-lua test-integration

test-rust:
	cargo test --workspace

test-lua: deps
	$(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua

test-integration: deps debug-bin
	REVIEWKIT_REAL_BIN=$(CURDIR)/$(DEBUG_BIN) $(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua

debug-bin:
	cargo build

check: check-rust check-lua

check-rust:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

# lua-language-server reads $VIMRUNTIME from .luarc.json; CI shells do not set it.
check-lua: export VIMRUNTIME ?= $(shell $(NVIM) --clean --headless +'echo $$VIMRUNTIME' +q 2>&1)
check-lua: deps debug-bin
	stylua --check lua plugin tests scripts
	lua-language-server --check . --checklevel=Warning
	$(NVIM) -l scripts/gen-types.lua --check --bin $(DEBUG_BIN)

fmt:
	cargo fmt --all
	stylua lua plugin tests scripts

gen-types: debug-bin
	$(NVIM) -l scripts/gen-types.lua --bin $(DEBUG_BIN)
