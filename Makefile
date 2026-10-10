.DEFAULT_GOAL := help

# A throwaway config dir, so screenshots never touch your settings.
SCREENSHOT_ENV := XDG_CONFIG_HOME=$(CURDIR)/target/screenshot-config XDG_DATA_HOME=$(CURDIR)/target/screenshot-data

.PHONY: help run dev build test check screenshot clean

help: ## Show this help
	@echo "Usage: make <target>"
	@echo
	@awk 'BEGIN { FS = ":.*## " } /^[a-z-]+:.*## / { printf "  \033[1m%-14s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

run: ## Run Notebook on the example notes
	cargo run -- examples/notes

dev: ## Run with live reload (requires cargo-watch)
	@command -v cargo-watch >/dev/null 2>&1 || { echo "cargo-watch is required: cargo install cargo-watch"; exit 1; }
	cargo watch -x "run -- examples/notes"

build: ## Build a release binary
	cargo build --release

test: ## Run the tests
	cargo test

check: test ## Run the tests and clippy
	cargo clippy --all-targets -- -D warnings

screenshot: ## Regenerate docs/screenshot-*.png from the example notes
	@rm -rf target/screenshot-config target/screenshot-data
	@mkdir -p docs target/screenshot-config target/screenshot-data
	$(SCREENSHOT_ENV) NOTEBOOK_SCREENSHOT=docs/screenshot-light.png NOTEBOOK_THEME=light cargo run -- examples/notes
	$(SCREENSHOT_ENV) NOTEBOOK_SCREENSHOT=docs/screenshot-dark-orange.png NOTEBOOK_SKIN="Dark Orange" cargo run -- examples/notes

clean: ## Remove build output
	cargo clean
