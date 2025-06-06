#!/usr/bin/env just --justfile

@_default:
    just --list

# Default build
build:
    cargo build --workspace --all-targets

# Quick compile
check:
    cargo check --workspace --all-targets

# Verify that the current version of the crate is not the same as the one published on crates.io
check-if-published:
    #!/usr/bin/env bash
    LOCAL_VERSION="$(grep '^version =' Cargo.toml | sed -E 's/version = "([^"]*)".*/\1/')"
    echo "Detected crate version:  $LOCAL_VERSION"
    CRATE_NAME="$(grep '^name =' Cargo.toml | head -1 | sed -E 's/name = "(.*)"/\1/')"
    echo "Detected crate name:     $CRATE_NAME"
    PUBLISHED_VERSION="$(cargo search ${CRATE_NAME} | grep "^${CRATE_NAME} =" | sed -E 's/.* = "(.*)".*/\1/')"
    echo "Published crate version: $PUBLISHED_VERSION"
    if [ "$LOCAL_VERSION" = "$PUBLISHED_VERSION" ]; then
        echo "ERROR: The current crate version has already been published."
        exit 1
    else
        echo "The current crate version has not yet been published."
    fi

# Run all tests as expected by CI
ci-test: rust-info test-fmt clippy test test-doc

# Clean all build artifacts
clean:
    cargo clean
    rm -f Cargo.lock

# Run cargo clippy
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Build and open code documentation
docs:
    cargo doc --no-deps --open

# Run cargo fmt
fmt:
    cargo +nightly fmt -- --config imports_granularity=Module,group_imports=StdExternalCrate

rust-info:
    rustc --version
    cargo --version

# Run all tests
test: build
    cargo test --workspace --all-targets

# Test documentation
test-doc:
    cargo doc --no-deps

# Test code formatting
test-fmt:
    cargo fmt --all -- --check

# Update dependencies, including breaking changes
update:
    cargo +nightly -Z unstable-options update --breaking
    cargo update
