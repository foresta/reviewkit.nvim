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
RUST_VERSION := 1.99.0
NVIM_VERSION := v0.12.5
# Only these spawn the real binary; the rest already ran in test-lua.
INTEGRATION_TESTS := tests/test_integration.lua tests/test_types.lua

build:
	cargo build --release --locked

# The stamp names the version, so changing MINI_VERSION clones again instead
# of testing against whatever was cloned first.
MINI_STAMP := .tests/deps/mini.nvim.$(MINI_VERSION)

deps: $(MINI_STAMP)

$(MINI_STAMP):
	rm -rf .tests/deps/mini.nvim .tests/deps/mini.nvim.*
	git clone --depth 1 --branch $(MINI_VERSION) https://github.com/echasnovski/mini.nvim .tests/deps/mini.nvim
	touch $@

test: test-rust test-lua test-integration

test-rust:
	cargo test --workspace --locked

# The tests run git, directly and through the CLI; the developer's global
# and system config (commit signing, hooks) must not reach it, as in the
# Rust tests.
ISOLATED_GIT = GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

test-lua: deps
	$(ISOLATED_GIT) $(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua

test-integration: deps debug-bin
	$(ISOLATED_GIT) REVIEWKIT_REAL_BIN=$(CURDIR)/$(DEBUG_BIN) $(NVIM) --headless -u scripts/minimal_init.lua -l scripts/run_tests.lua $(INTEGRATION_TESTS)

debug-bin:
	cargo build --locked

check: check-rust check-lua

check-rust:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

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
	@echo "rust=$(RUST_VERSION)"
	@echo "nvim=$(NVIM_VERSION)"
