.PHONY: help install install-cli install-skills

.DEFAULT_GOAL := help

help:
	@echo "make install         install the autoground binary, then the skills"
	@echo "make install-cli     cargo install --path . --locked --force"
	@echo "make install-skills  copy skills into detected agents (INSTALL_FLAGS=--dry-run)"

install: install-cli install-skills

install-cli:
	@need=$$(awk -F '"' '/^rust-version[[:space:]]*=/ { print $$2; exit }' Cargo.toml); \
	if [ -z "$$need" ]; then \
		echo "Cargo.toml has no rust-version." >&2; \
		exit 1; \
	fi; \
	if ! command -v rustc >/dev/null 2>&1; then \
		echo "rustc was not found. autoground requires Rust $$need or newer: https://rustup.rs" >&2; \
		exit 1; \
	fi; \
	have=$$(rustc --version | awk '{ print $$2 }' | cut -d- -f1); \
	if ! printf '%s\n%s\n' "$$need" "$$have" | awk -F. ' \
		NR == 1 { for (i = 1; i <= NF; i++) need[i] = $$i + 0; nlen = NF; next } \
		{ for (i = 1; i <= NF; i++) have[i] = $$i + 0; hlen = NF } \
		END { \
			len = (nlen > hlen ? nlen : hlen); \
			for (i = 1; i <= len; i++) { \
				if (have[i] + 0 < need[i] + 0) exit 1; \
				if (have[i] + 0 > need[i] + 0) exit 0; \
			} \
			exit 0; \
		}'; then \
		echo "autoground requires Rust $$need or newer (this is rustc $$have). Update with: rustup update" >&2; \
		exit 1; \
	fi
	cargo install --path . --locked --force

install-skills:
	./install.sh $(INSTALL_FLAGS)
