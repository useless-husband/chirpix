# All paths are relative: the repository may live in a directory whose name
# contains spaces or non-ASCII characters.
CARGO ?= cargo
JOBS ?= 4
BIN := ./target/release/chirpix
KODAK := data/kodim23.png data/kodim19.png data/kodim05.png data/kodim08.png

.PHONY: build test lint demo report data loopback clean

build:
	$(CARGO) build --release -j $(JOBS)

# Unit tests plus the end-to-end tests through the simulated channel (a few seconds once compiled).
test:
	$(CARGO) test --release -j $(JOBS)

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --release -j $(JOBS) --all-targets -- -D warnings
	sh -n scripts/fetch_data.sh
	sh -n scripts/loopback.sh
	bash -n "跑跑看.command"

# Small version of the report on built-in test pictures (what CI runs, under a minute).
demo: build
	$(BIN) report --quick -o out/demo --threads $(JOBS)

# Four Kodak photographs, checked by SHA-256, into data/ (not in the repository).
data:
	sh scripts/fetch_data.sh

# The full report: on the Kodak photographs if `make data` was run,
# otherwise on the built-in pictures. About 90 s on four cores.
report: build
	@if [ -f data/kodim23.png ]; then $(BIN) report -o out/report --threads $(JOBS) $(KODAK); \
	else $(BIN) report -o out/report --threads $(JOBS); fi

# Real audio stack, no sound: play into and record from the BlackHole virtual device.
loopback: build
	sh scripts/loopback.sh || [ $$? -eq 77 ]

clean:
	$(CARGO) clean
	rm -rf out
