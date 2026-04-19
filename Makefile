# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# Top-level Makefile for the most-used dev workflows. The build system
# is Cargo + maturin; this is just convenience targets for things that
# don't slot naturally into either tool.

.PHONY: help regen-schema check-schema test test-py test-rust

help:
	@echo "Available targets:"
	@echo "  regen-schema   Regenerate Python TypedDicts from the Rust DispatchRequest schema"
	@echo "  check-schema   Fail (exit 1) if the generated TypedDicts are stale"
	@echo "  test           cargo test (excluding surge-py) + uv run pytest"
	@echo "  test-rust      cargo test --workspace --exclude surge-py"
	@echo "  test-py        uv run pytest src/surge-py/tests/ tests/"

# ---------------------------------------------------------------------------
# Schema codegen
#
# `regen-schema` runs the Rust ``emit-schema`` bin (printing the
# DispatchRequest JSON schema) and pipes it through the Python
# generator that produces ``surge/_generated/dispatch_request.py``.
# `check-schema` does the same but fails if the output would change —
# wire this into CI to keep the generated file in sync with the Rust
# source of truth.
# ---------------------------------------------------------------------------

regen-schema:
	python3 scripts/codegen_dispatch_request.py

check-schema:
	python3 scripts/codegen_dispatch_request.py --check

# ---------------------------------------------------------------------------
# Test runners (mirror the commands in CLAUDE.md)
# ---------------------------------------------------------------------------

test: test-rust test-py

test-rust:
	cargo test --workspace --exclude surge-py --lib --tests

test-py:
	uv run pytest src/surge-py/tests/ tests/ -q --tb=short
