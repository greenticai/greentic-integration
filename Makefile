.DEFAULT_GOAL := help

SHELL := /bin/bash

SCRIPTS_DIR := scripts
LOG_DIR := .logs
COMPOSE ?= docker compose
STACK_FILE := compose/stack.yml

.PHONY: help stack-up stack-down packs.test runner.smoke render.snapshot webchat.e2e dev.min dev.full webchat.contract golden.update app.test
.PHONY: test.all

help: ## Show available commands
	@printf "\nGreentic Integration Make targets\n\n"
	@grep -E '^[a-zA-Z0-9_.-]+:.*?##' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "} {printf "  %-20s %s\n", $$1, $$2}'
	@printf "\nTargets marked as TODO will be implemented in later PR-INT phases.\n\n"

stack-up: ## Provision the local infra stack (PR-INT-03)
	@$(COMPOSE) -f $(STACK_FILE) up -d --remove-orphans
	@$(COMPOSE) -f $(STACK_FILE) ps

stack-down: ## Tear down the local infra stack (PR-INT-03)
	@$(COMPOSE) -f $(STACK_FILE) down -v

packs.test: ## Validate pack fixtures (PR-INT-04)
	@$(SCRIPTS_DIR)/packs_test.py

runner.smoke: ## Run runner smoke tests (PR-INT-06)
	@cargo run -p runner-smoke -- --cases harness/runner-smoke/cases

app.test: ## Run app crate unit tests (session/resume/runner stubs)
	@GREENTIC_PROVIDER_CORE_ONLY=1 cargo test -p greentic-integration

render.snapshot: ## Capture renderer snapshots (PR-INT-05)
	@cargo test -p providers-sim -- render_reports_match_golden

webchat.contract: ## Run WebChat backend contract tests (PR-INT-07)
	@$(SCRIPTS_DIR)/webchat_contract.py

webchat.e2e: ## Run WebChat Playwright E2E suite (PR-INT-08)
	@$(SCRIPTS_DIR)/run_webchat_e2e.sh

test.all: ## Run full integration test suite (packs + app + smoke + renderer + webchat)
	@set -euo pipefail; \
	$(MAKE) packs.test; \
	$(MAKE) app.test; \
	$(MAKE) runner.smoke; \
	$(MAKE) render.snapshot; \
	$(MAKE) webchat.contract; \
	$(MAKE) webchat.e2e;

test.summary: ## Run all suites and print a consolidated pass/fail summary
	@bash $(SCRIPTS_DIR)/run_all_tests.sh

component.deploy-plan: ## Build the deploy-plan guest component and copy it into packs/deploy-generic
	@$(SCRIPTS_DIR)/build_deploy_component.sh

dev.min: ## Minimal developer bootstrap (PR-INT-02)
	@$(SCRIPTS_DIR)/dev_stub.sh dev.min 'Minimal dev bootstrap stub complete. Replace with real flow in PR-INT-02 follow-ups.'
	@./scripts/dev-check/check.sh

dev.full: dev.min ## Full developer bootstrap (PR-INT-02)
	@$(SCRIPTS_DIR)/dev_stub.sh dev.full 'Full dev bootstrap stub complete. Replace with real flow post PR-INT-02.'

golden.update: ## Stub: Refresh golden snapshots (PR-INT-10)
	@$(SCRIPTS_DIR)/update_golden.sh

.PHONY: demo.replay.build demo.replay.chat
demo.replay.build: ## Replay sample build-status payload via runner emit (local stub)
	@./scripts/replay_build_status_payload.sh

demo.replay.chat: ## Replay sample chat payload via runner emit (local stub)
	@./scripts/replay_repo_assistant_payload.sh
