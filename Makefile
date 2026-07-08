.PHONY: build run test install docker lint fmt clean

BINARY = llm-retry-proxy
TARGET_DIR = target/release

build:
	cargo build --release

run:
	cargo run -- --config config.toml --log-level info

test:
	cargo test

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

install: build
	@echo "Installing $(BINARY)..."
	@read -p "Install system service? [y/N] " install_service; \
	if [ "$$install_service" = "y" ] || [ "$$install_service" = "Y" ]; then \
		$(MAKE) install-service; \
	fi
	@echo "Copying binary to /usr/local/bin..."
	@cp $(TARGET_DIR)/$(BINARY) /usr/local/bin/
	@echo "Installation complete."
	@echo "Create a config file at /etc/llm-retry-proxy/config.toml (see config.example.toml)"
	@echo "Start with: llm-retry-proxy --config /etc/llm-retry-proxy/config.toml"

install-service:
	@if command -v systemctl >/dev/null 2>&1; then \
		echo "Detected systemd, installing service..."; \
		mkdir -p /etc/llm-retry-proxy; \
		cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		cp systemd/llm-retry-proxy.service /etc/systemd/system/; \
		systemctl daemon-reload; \
		echo "Enable and start with: systemctl enable --now llm-retry-proxy"; \
	elif command -v launchctl >/dev/null 2>&1; then \
		echo "Detected launchd, installing service..."; \
		mkdir -p /etc/llm-retry-proxy; \
		cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		cp systemd/llm-retry-proxy.plist /Library/LaunchDaemons/; \
		echo "Load with: launchctl load /Library/LaunchDaemons/llm-retry-proxy.plist"; \
	else \
		echo "No supported service manager found. Skipping service installation."; \
	fi

docker:
	docker build -t $(BINARY) .

clean:
	cargo clean
