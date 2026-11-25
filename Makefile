PREFIX ?= $(HOME)/.local

.PHONY: check update
check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

update:
	cargo install --locked --path . --root "$(PREFIX)" --force
	"$(PREFIX)/bin/veruna-pack" --version
