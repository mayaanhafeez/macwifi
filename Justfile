default: build

# Build the binary (debug).
build:
    cargo build

# Build the optimized binary.
release:
    cargo build --release

# Build + assemble the .app bundle. Run after `just release`.
bundle: release
    ./scripts/bundle.sh

# Install a symlink to /usr/local/bin/macwifi (requires sudo on some setups).
install: bundle
    ln -sf "$(pwd)/target/release/macwifi.app/Contents/MacOS/macwifi" /usr/local/bin/macwifi

# Remove the symlink and bundle.
clean-install:
    rm -f /usr/local/bin/macwifi
    rm -rf target/release/macwifi.app

# Run the TUI from the bundled binary (so Location works).
run: bundle
    ./target/release/macwifi.app/Contents/MacOS/macwifi

themes:
    ./target/release/macwifi themes

fmt:
    cargo fmt

check:
    cargo check
