.PHONY: help install install-cli install-skills

.DEFAULT_GOAL := help

help:
	@echo "make install         install the autoground binary, then the skills"
	@echo "make install-cli     cargo install --path . --locked --force"
	@echo "make install-skills  link skills into detected agents (INSTALL_FLAGS=--dry-run)"

install: install-cli install-skills

install-cli:
	cargo install --path . --locked --force

install-skills:
	./install.sh $(INSTALL_FLAGS)
