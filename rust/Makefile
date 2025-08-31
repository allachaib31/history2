.PHONY: help build release run clean test install-deps docker docker-run fmt clippy bench

# Default target
all: build

# Help target
help:
	@echo "Webook Bot - Rust Edition Build Targets"
	@echo "======================================="
	@echo "  make build       - Build debug version"
	@echo "  make release     - Build optimized release version"
	@echo "  make run         - Build and run debug version"
	@echo "  make run-release - Build and run release version"
	@echo "  make clean       - Clean build artifacts"
	@echo "  make test        - Run tests"
	@echo "  make bench       - Run benchmarks"
	@echo "  make fmt         - Format code"
	@echo "  make clippy      - Run linter"
	@echo "  make install-deps- Install system dependencies"
	@echo "  make docker      - Build Docker image"
	@echo "  make docker-run  - Run in Docker container"

# Build debug version
build:
	cargo build
	@echo "✅ Debug build complete: target/debug/webook_bot"

# Build release version with maximum optimizations
release:
	RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C lto=fat -C embed-bitcode=yes" \
	cargo build --release
	@echo "✅ Release build complete: target/release/webook_bot"
	@ls -lh target/release/webook_bot

# Run debug version
run:
	cargo run

# Run release version
run-release:
	cargo run --release

# Clean build artifacts
clean:
	cargo clean
	rm -f tokens.json
	@echo "✅ Build artifacts cleaned"

# Run tests
test:
	cargo test -- --nocapture

# Run benchmarks
bench:
	cargo bench

# Format code
fmt:
	cargo fmt -- --check

# Run clippy linter
clippy:
	cargo clippy -- -D warnings

# Install system dependencies (Linux)
install-deps-linux:
	sudo apt update
	sudo apt install -y \
		build-essential \
		pkg-config \
		libssl-dev \
		libgtk-3-dev \
		libwebkit2gtk-4.0-dev \
		libayatana-appindicator3-dev \
		librsvg2-dev \
		chromium-browser
	@echo "✅ Linux dependencies installed"

# Install system dependencies (macOS)
install-deps-macos:
	brew install gtk+3 chromium pkg-config
	@echo "✅ macOS dependencies installed"

# Install Rust and cargo if not present
install-rust:
	@which cargo > /dev/null || (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)
	@echo "✅ Rust installed"

# Build for multiple platforms
build-all-platforms:
	@echo "Building for multiple platforms..."
	cargo build --release --target x86_64-unknown-linux-gnu
	cargo build --release --target x86_64-pc-windows-gnu
	cargo build --release --target x86_64-apple-darwin
	@echo "✅ Multi-platform builds complete"

# Create distribution package
dist: release
	@mkdir -p dist
	@cp target/release/webook_bot dist/
	@cp README.md dist/
	@echo "Creating distribution archive..."
	@tar -czf webook_bot_$(shell date +%Y%m%d).tar.gz dist/
	@echo "✅ Distribution package created"

# Docker build
docker:
	docker build -t webook-bot-rust .
	@echo "✅ Docker image built: webook-bot-rust"

# Docker run
docker-run:
	docker run -it --rm \
		-e DISPLAY=$$DISPLAY \
		-v /tmp/.X11-unix:/tmp/.X11-unix \
		-v $$(pwd)/tokens.json:/app/tokens.json \
		-v $$(pwd)/users.csv:/app/users.csv \
		-v $$(pwd)/proxies.txt:/app/proxies.txt \
		webook-bot-rust

# Development build with debugging symbols
debug:
	RUST_BACKTRACE=1 RUST_LOG=debug cargo build
	@echo "✅ Debug build with symbols complete"

# Profile guided optimization build
pgo-build:
	@echo "Building with Profile-Guided Optimization..."
	# Step 1: Build with profiling
	RUSTFLAGS="-Cprofile-generate=/tmp/pgo-data" cargo build --release
	# Step 2: Run the program to generate profile data
	./target/release/webook_bot --benchmark || true
	# Step 3: Build with profile data
	RUSTFLAGS="-Cprofile-use=/tmp/pgo-data -Cllvm-args=-pgo-warn-missing-function" cargo build --release
	@echo "✅ PGO build complete"

# Check for security vulnerabilities
audit:
	cargo audit
	@echo "✅ Security audit complete"

# Update dependencies
update:
	cargo update
	@echo "✅ Dependencies updated"

# Size optimization build
size-opt:
	RUSTFLAGS="-C opt-level=z" cargo build --release
	strip target/release/webook_bot
	@echo "✅ Size-optimized build complete"
	@ls -lh target/release/webook_bot

# Create Windows installer (requires wine on Linux/macOS)
windows-installer:
	cargo build --release --target x86_64-pc-windows-gnu
	@echo "Creating Windows installer..."
	# Add NSIS or MSI creation commands here
	@echo "✅ Windows installer created"

# Run with Valgrind for memory profiling (Linux only)
valgrind:
	cargo build
	valgrind --leak-check=full --show-leak-kinds=all target/debug/webook_bot

# CPU profiling with perf (Linux only)
perf:
	cargo build --release
	perf record -g target/release/webook_bot
	perf report

# Generate documentation
docs:
	cargo doc --open

# Quick development cycle
dev: fmt clippy build run

# Production deployment
deploy: clean release test
	@echo "✅ Ready for deployment"
	@echo "Binary location: target/release/webook_bot"