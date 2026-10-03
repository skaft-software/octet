# Reusable C++17 executable template; same Rust archive/C ABI as the C recipe.
OCTET_ROOT ?= ../..
MAIN ?= $(OCTET_ROOT)/examples/extensions/native-hello/cpp/main.cpp
OUTPUT ?= $(OCTET_ROOT)/examples/extensions/native-hello/build/hello-cpp-template
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
	$(CXX) -std=c++17 -Wall -Wextra -Werror $(CXXFLAGS) -I"$(OCTET_ROOT)/sdk/c/include" -I"$(OCTET_ROOT)/sdk/cpp/include" "$(MAIN)" "$(ARCHIVE)" $(LDFLAGS) $(LDLIBS) -o "$(OUTPUT)"
runtime:
	CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 CARGO_TARGET_DIR="$(TARGET_DIR)" cargo build --manifest-path "$(OCTET_ROOT)/sdk/rust/Cargo.toml" --offline --locked --lib
