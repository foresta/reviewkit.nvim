# debug-bin is phony on purpose: cargo, not make, knows when the sources
# changed, so a file target would keep testing a stale binary.
.PHONY: print-versions build debug-bin dist dist-sums deps test test-rust test-lua test-integration check check-rust check-lua fmt gen-types gen-schema

NVIM ?= nvim
DEBUG_BIN := target/debug/reviewkit
# Pinned so a new release of a test or lint tool cannot turn the gate red
# on its own; CI reads the same values.
MINI_VERSION := v0.18.0
STYLUA_VERSION := 2.6.0
LUALS_VERSION := 3.19.2
# Only these spawn the real binary; the rest already ran in test-lua.
INTEGRATION_TESTS := tests/test_integration.lua tests/test_types.lua

build:
	cargo build --release

deps: .tests/deps/mini.nvim

.tests/deps/mini.nvim:
	git clone --depth 1 --branch $(MINI_VERSION) https://github.com/echasnovski/mini.nvim $@

test: test-rust test-lua test-integration

test-rust:
	cargo test --workspace

test-lua: deps
	$(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua

test-integration: deps debug-bin
	REVIEWKIT_REAL_BIN=$(CURDIR)/$(DEBUG_BIN) $(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua $(INTEGRATION_TESTS)

debug-bin:
	cargo build

check: check-rust check-lua

check-rust:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

# lua-language-server reads $VIMRUNTIME from .luarc.json; CI shells do not set it.
check-lua: export VIMRUNTIME ?= $(shell $(NVIM) --clean --headless +'echo $$VIMRUNTIME' +q 2>&1)
check-lua: deps debug-bin
	@stylua --version | grep -q " $(STYLUA_VERSION)$$" || echo "warning: stylua $(STYLUA_VERSION) is the pinned version" >&2
	stylua --check lua plugin tests scripts
	lua-language-server --check . --checklevel=Warning
	$(NVIM) -l scripts/gen-types.lua --check --bin $(DEBUG_BIN)

fmt:
	cargo fmt --all
	stylua lua plugin tests scripts

gen-types: debug-bin
	$(NVIM) -l scripts/gen-types.lua --bin $(DEBUG_BIN)

gen-schema:
	REVIEWKIT_UPDATE_SCHEMA=1 cargo test --test schema

TARGET ?= $(shell rustc -vV | sed -n 's/^host: //p')

dist:
	cargo build --release --locked --target $(TARGET)
	mkdir -p dist
	cp target/$(TARGET)/release/reviewkit dist/reviewkit-$(TARGET)

dist-sums:
	cd dist && shasum -a 256 reviewkit-* > SHA256SUMS

# For CI: one source of truth for the pinned tool versions.
print-versions:
	@echo "stylua=$(STYLUA_VERSION)"
	@echo "luals=$(LUALS_VERSION)"
