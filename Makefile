BIN := $(HOME)/.local/bin/imi

# Build from source and install locally — your "localhost" for testing
install:
	cargo build --release
	cp target/release/imi $(BIN)
	@echo "Installed: $$($(BIN) --version)"

# Just build, don't install
build:
	cargo build --release

# Run integration tests
test:
	bash tests/integration.sh

# Run benchmark integration tests
test-benchmark:
	bash tests/benchmark-integration.sh

# Run all tests
test-all: test test-benchmark

# Run benchmark smoke test (1 run each of baseline and treatment)
benchmark-smoke:
	@echo "=== Benchmark Smoke Test ==="
	./scripts/benchmark-runner.sh --mode baseline --runs 1 --seed 999
	./scripts/benchmark-runner.sh --mode treatment --runs 1 --seed 999 --memory-injection
	@echo ""
	@echo "✓ Smoke test complete. Check benchmark-results/ for output."

# Clean benchmark results
benchmark-clean:
	rm -rf benchmark-results/

.PHONY: install build test test-benchmark test-all benchmark-smoke benchmark-clean
