.PHONY: build run test install update docker lint fmt fmt-check clean

BINARY = llm-retry-proxy
TARGET_DIR = target/release

build:
	cargo build --release

run:
	cargo run -- --config config.toml --log-level info

# Update: rebuild and replace the binary in PATH (for human verification after AI dev)
update: build
	@CURRENT_BIN=$$(which $(BINARY) 2>/dev/null) || { echo "$(BINARY) not found in PATH. Run 'make install' first."; exit 1; }; \
	echo "Updating $$CURRENT_BIN..."; \
	cp $(TARGET_DIR)/$(BINARY) "$$CURRENT_BIN"; \
	chmod +x "$$CURRENT_BIN"; \
	if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active $(BINARY) >/dev/null 2>&1; then \
		systemctl --user restart $(BINARY); \
		echo "Restarted user service."; \
	elif command -v systemctl >/dev/null 2>&1 && systemctl is-active $(BINARY) >/dev/null 2>&1; then \
		sudo systemctl restart $(BINARY); \
		echo "Restarted system service."; \
	fi; \
	echo "Update complete: $$CURRENT_BIN → $$(./$(TARGET_DIR)/$(BINARY) --version 2>&1 | head -1)"

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
	@echo ""
	@echo "Choose binary install path:"
	@echo "  1) /usr/local/bin (system-wide, requires sudo)"
	@echo "  2) ~/.local/bin (user-level)"
	@echo "  3) Custom path"
	@read -p "Select [1-3, default=2]: " bin_choice; \
	case "$$bin_choice" in \
		1) BIN_DIR=/usr/local/bin ;; \
		3) read -p "Enter custom path: " custom_bin; BIN_DIR="$$custom_bin" ;; \
		*) BIN_DIR=$(HOME)/.local/bin ;; \
	esac; \
	mkdir -p "$$BIN_DIR"; \
	cp $(TARGET_DIR)/$(BINARY) "$$BIN_DIR/"; \
	chmod +x "$$BIN_DIR/$(BINARY)"; \
	echo "Binary installed to: $$BIN_DIR/$(BINARY)"; \
	BIN_PATH="$$BIN_DIR/$(BINARY)"; \
	echo ""; \
	echo "Install system service?"; \
	echo "  1) Yes, system-wide (requires sudo)"; \
	echo "  2) Yes, user-level (~/.config/systemd/user)"; \
	echo "  3) No"; \
	read -p "Select [1-3, default=3]: " svc_choice; \
	case "$$svc_choice" in \
		1) $(MAKE) install-service-system BIN_PATH="$$BIN_PATH" ;; \
		2) $(MAKE) install-service-user BIN_PATH="$$BIN_PATH" ;; \
		*) echo "Skipping service installation." ;; \
	esac; \
	echo ""; \
	echo "=== Installation complete ==="

install-service-system: BIN_PATH :=
install-service-system:
	@echo "Installing system-wide systemd service..."
	@if command -v systemctl >/dev/null 2>&1; then \
		sudo mkdir -p /etc/llm-retry-proxy; \
		sudo cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		sed -e 's|__BIN_PATH__|$(BIN_PATH)|' \
		    -e 's|__CONFIG_PATH__|/etc/llm-retry-proxy/config.toml|' \
		    systemd/llm-retry-proxy.service > /tmp/llm-retry-proxy.service; \
		sudo cp /tmp/llm-retry-proxy.service /etc/systemd/system/; \
		rm -f /tmp/llm-retry-proxy.service; \
		sudo systemctl daemon-reload; \
		echo "Service file: /etc/systemd/system/llm-retry-proxy.service"; \
		echo "Config file:  /etc/llm-retry-proxy/config.toml"; \
		echo ""; \
		echo "Enable and start with:"; \
		echo "  sudo systemctl enable --now llm-retry-proxy"; \
		echo "  sudo systemctl status llm-retry-proxy"; \
	elif command -v launchctl >/dev/null 2>&1; then \
		sudo mkdir -p /etc/llm-retry-proxy; \
		sudo cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		sed -e 's|__BIN_PATH__|$(BIN_PATH)|' \
		    -e 's|__CONFIG_PATH__|/etc/llm-retry-proxy/config.toml|' \
		    systemd/llm-retry-proxy.plist > /tmp/llm-retry-proxy.plist; \
		sudo cp /tmp/llm-retry-proxy.plist /Library/LaunchDaemons/; \
		rm -f /tmp/llm-retry-proxy.plist; \
		echo "Service file: /Library/LaunchDaemons/llm-retry-proxy.plist"; \
		echo "Config file:  /etc/llm-retry-proxy/config.toml"; \
		echo ""; \
		echo "Load with:"; \
		echo "  sudo launchctl load /Library/LaunchDaemons/llm-retry-proxy.plist"; \
	else \
		echo "No supported service manager found. Skipping."; \
	fi

install-service-user: BIN_PATH :=
install-service-user:
	@echo "Installing user-level systemd service..."
	@if command -v systemctl >/dev/null 2>&1; then \
		CONFIG_DIR="$(HOME)/.config/llm-retry-proxy"; \
		SERVICE_DIR="$(HOME)/.config/systemd/user"; \
		mkdir -p "$$CONFIG_DIR" "$$SERVICE_DIR"; \
		cp config.example.toml "$$CONFIG_DIR/config.toml"; \
		CONFIG_PATH="$$CONFIG_DIR/config.toml"; \
		sed -e "s|__BIN_PATH__|$(BIN_PATH)|" \
		    -e "s|__CONFIG_PATH__|$$CONFIG_PATH|" \
		    -e "s|multi-user.target|default.target|" \
		    systemd/llm-retry-proxy.service > "$$SERVICE_DIR/llm-retry-proxy.service"; \
		systemctl --user daemon-reload; \
		echo "Service file: $$SERVICE_DIR/llm-retry-proxy.service"; \
		echo "Config file:  $$CONFIG_PATH"; \
		echo ""; \
		echo "Enable and start with:"; \
		echo "  systemctl --user enable --now llm-retry-proxy"; \
		echo "  systemctl --user status llm-retry-proxy"; \
		echo ""; \
		echo "Note: For service to survive logout, run:"; \
		echo "  loginctl enable-linger $$USER"; \
	else \
		echo "systemctl not found. User-level systemd not available. Skipping."; \
	fi

docker:
	docker build -t $(BINARY) .

clean:
	cargo clean
