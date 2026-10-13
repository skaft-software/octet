# Reusable C11 executable template. Set OCTET_ROOT to the source checkout and
# MAIN/OUTPUT to your one authoring source and executable. Rust still needs to be
# built; this is not arbitrary C plugin loading or a toolchain-free SDK.
OCTET_ROOT ?= ../..
MAIN ?= $(OCTET_ROOT)/examples/extensions/native-hello/c/main.c
OUTPUT ?= $(OCTET_ROOT)/examples/extensions/native-hello/build/hello-c-template
TARGET_DIR ?= $(abspath $(OCTET_ROOT)/sdk/rust/target)
ARCHIVE = $(TARGET_DIR)/debug/liboctet_extension.a
SYSTEM = $(shell uname -s)
ifeq ($(SYSTEM),Darwin)
LDLIBS += -lpthread -ldl -lm -liconv -framework Security -framework CoreFoundation -framework SystemConfiguration
else ifeq ($(SYSTEM),Linux)
LDLIBS += -lpthread -ldl -lm -lrt -lutil
else
$(error Unqualified platform; use a reviewed native toolchain/link recipe)
endif
.PHONY: all runtime
all: runtime
	mkdir -p "$(dir $(OUTPUT))"
	$(CC) -std=c11 -Wall -Wextra -Werror $(CFLAGS) -I"$(OCTET_ROOT)/sdk/c/include" "$(MAIN)" "$(ARCHIVE)" $(LDFLAGS) $(LDLIBS) -o "$(OUTPUT)"
runtime:
	CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 CARGO_TARGET_DIR="$(TARGET_DIR)" cargo build --manifest-path "$(OCTET_ROOT)/sdk/rust/Cargo.toml" --offline --locked --lib
