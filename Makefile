# Shortcuts for common httpdog commands. The build logic lives in cargo; this only wraps the commands
# listed in README.md and docs/development.md so they need not be retyped. Plain `make` lists every target.

BIN    := target/release/httpdog
CONFIG := rules.example.yaml
SAMPLE := sample.log

# Rule files check-rules validates: the main example plus the scenario templates under examples/
RULE_FILES := $(CONFIG) $(wildcard examples/*.yaml)

# The example rules refer to HTTPDOG_WEBHOOK_URL and HTTPDOG_WEBHOOK_SECRET. Validation and smoke tests always
# override the environment with these dummy values, so a real webhook set on a dev machine never gets test alerts.
DUMMY_URL := http://127.0.0.1:9/x
DUMMY_ENV := HTTPDOG_WEBHOOK_URL=$(DUMMY_URL) HTTPDOG_WEBHOOK_SECRET=dummy-secret-for-tests

# Number of alerts sample.log should fire with the example rules; change it together with either file
EXPECTED_ALERTS := 3

# The combined pair: the starter template with sample.combined.log
COMBINED_CONFIG := examples/00-combined-starter.yaml
COMBINED_SAMPLE := sample.combined.log
COMBINED_EXPECTED_ALERTS := 4

# Replay a log and check the number of ALERT lines. Usage: $(call replay,rule file,log file,expected ALERT count)
define replay
@out=$$($(DUMMY_ENV) $(BIN) --stdin --config $(1) < $(2) 2>&1); \
	status=$$?; \
	printf '%s\n' "$$out"; \
	if [ $$status -ne 0 ]; then \
		echo "smoke test failed ($(2)): exit status $$status" >&2; exit 1; \
	fi; \
	n=$$(printf '%s\n' "$$out" | grep -c '^ALERT'); \
	if [ "$$n" -ne $(3) ]; then \
		echo "smoke test failed ($(2)): expected $(3) ALERT lines, got $$n" >&2; exit 1; \
	fi; \
	echo "smoke test passed ($(2)): $$n ALERT lines"
endef

.DEFAULT_GOAL := help
.PHONY: help build test lint fmt fmt-check check-rules smoke ci fuzz clean

help: ## List the available targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  make %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build the release binary (target/release/httpdog)
	cargo build --release

test: ## Run the unit tests
	cargo test

lint: ## clippy; any warning fails
	cargo clippy --all-targets -- -D warnings

fmt: ## Format the code with rustfmt
	cargo fmt

fmt-check: ## Check formatting without changing files
	cargo fmt --check

check-rules: build ## Check that rules.example.yaml and the rule files under examples/ pass httpdog -t
	@for f in $(RULE_FILES); do \
		$(DUMMY_ENV) $(BIN) -t --config "$$f" 2>&1 || exit 1; \
	done

smoke: build ## End-to-end smoke test: replay the JSON and combined sample logs and count ALERT lines
	$(call replay,$(CONFIG),$(SAMPLE),$(EXPECTED_ALERTS))
	$(call replay,$(COMBINED_CONFIG),$(COMBINED_SAMPLE),$(COMBINED_EXPECTED_ALERTS))

ci: fmt-check lint test check-rules smoke ## Everything before a commit: format, clippy, tests, rule files, smoke test

# Seconds each fuzz target runs for. Needs the nightly toolchain and cargo-fuzz, see fuzz/run.sh
FUZZTIME ?= 60
fuzz: ## Fuzz the log parsers and the rule engine, every target at once (FUZZTIME=300 for longer)
	sh fuzz/run.sh $(FUZZTIME)

clean: ## Remove build output
	cargo clean
