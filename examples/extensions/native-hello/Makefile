# Local build template. No install/publish/enable target.
.PHONY: all test sanitize rust
all:
	bash ./build.sh

test: all
	python3 ../../../sdk/rust/tests/test_process.py

sanitize:
	SANITIZE=1 OCTET_NATIVE_BIN_DIR="$(CURDIR)/build-sanitized" bash ./build.sh
	OCTET_NATIVE_BIN_DIR="$(CURDIR)/build-sanitized" python3 ../../../sdk/rust/tests/test_process.py

rust:
	CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 cargo build --manifest-path rust/Cargo.toml --offline --locked
