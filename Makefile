.PHONY: build run test install update docker lint fmt fmt-check clean

BINARY = llm-retry-proxy
TARGET_DIR = target/release

build:
	cargo build --release

run:
	cargo run -- --config config.toml --log-level info

# Update: rebuild and replace the binary in PATH (for human verification after AI dev)
# Must stop service before cp to avoid "Text file busy", then start after.
update: build
	@CURRENT_BIN=$$(which $(BINARY) 2>/dev/null) || { echo "$(BINARY) not found in PATH. Run 'make install' first."; exit 1; }; \
	echo "Updating $$CURRENT_BIN..."; \
	if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active $(BINARY) >/dev/null 2>&1; then \
		SVC_TYPE=user; systemctl --user stop $(BINARY); \
	elif command -v systemctl >/dev/null 2>&1 && systemctl is-active $(BINARY) >/dev/null 2>&1; then \
		SVC_TYPE=system; sudo systemctl stop $(BINARY); \
	elif command -v launchctl >/dev/null 2>&1 && launchctl list $(BINARY) >/dev/null 2>&1; then \
		SVC_TYPE=launchd; launchctl unload ~/Library/LaunchAgents/$(BINARY).plist 2>/dev/null; \
	else SVC_TYPE=none; fi; \
	cp $(TARGET_DIR)/$(BINARY) "$$CURRENT_BIN"; \
	chmod +x "$$CURRENT_BIN"; \
	case "$$SVC_TYPE" in \
		user) systemctl --user start $(BINARY); echo "Restarted user systemd service." ;; \
		system) sudo systemctl start $(BINARY); echo "Restarted system systemd service." ;; \
		launchd) launchctl load ~/Library/LaunchAgents/$(BINARY).plist 2>/dev/null || sudo launchctl load /Library/LaunchDaemons/$(BINARY).plist 2>/dev/null; echo "Reloaded launchd service." ;; \
		none) echo "No service manager detected, binary updated in place." ;; \
	esac; \
	echo "Update complete: $$CURRENT_BIN → $$($(TARGET_DIR)/$(BINARY) --version 2>&1 | head -1)"

test:
	cargo test

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

# Detect service manager: "systemd", "launchd", or "none"
SERVICE_MANAGER := $(shell \
	if command -v systemctl >/dev/null 2>&1; then \
		echo systemd; \
	elif command -v launchctl >/dev/null 2>&1; then \
		echo launchd; \
	else \
		echo none; \
	fi)

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
	if [ "$(SERVICE_MANAGER)" = "systemd" ]; then \
		echo "Install systemd service?"; \
		echo "  1) Yes, system-wide (requires sudo)"; \
		echo "  2) Yes, user-level (~/.config/systemd/user)"; \
		echo "  3) No"; \
		read -p "Select [1-3, default=3]: " svc_choice; \
		case "$$svc_choice" in \
			1) $(MAKE) install-service-system BIN_PATH="$$BIN_PATH" ;; \
			2) $(MAKE) install-service-user BIN_PATH="$$BIN_PATH" ;; \
			*) echo "Skipping service installation." ;; \
		esac; \
	elif [ "$(SERVICE_MANAGER)" = "launchd" ]; then \
		echo "Install launchd service?"; \
		echo "  1) Yes, system-wide (requires sudo)"; \
		echo "  2) Yes, user-level (~/Library/LaunchAgents)"; \
		echo "  3) No"; \
		read -p "Select [1-3, default=3]: " svc_choice; \
		case "$$svc_choice" in \
			1) $(MAKE) install-service-system BIN_PATH="$$BIN_PATH" ;; \
			2) $(MAKE) install-service-user BIN_PATH="$$BIN_PATH" ;; \
			*) echo "Skipping service installation." ;; \
		esac; \
	else \
		echo "No supported service manager (systemd/launchd) found. Skipping service installation."; \
	fi; \
	echo ""; \
	echo "=== Installation complete ==="

install-service-system: BIN_PATH :=
install-service-system:
	@if [ "$(SERVICE_MANAGER)" = "systemd" ]; then \
		echo "Installing system-wide systemd service..."; \
		sudo mkdir -p /etc/llm-retry-proxy /var/log/llm-retry-proxy; \
		sudo cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		sed -e 's|__BIN_PATH__|$(BIN_PATH)|' \
		    -e 's|__CONFIG_PATH__|/etc/llm-retry-proxy/config.toml|' \
		    services/llm-retry-proxy.service > /tmp/llm-retry-proxy.service; \
		sudo cp /tmp/llm-retry-proxy.service /etc/systemd/system/; \
		rm -f /tmp/llm-retry-proxy.service; \
		sudo systemctl daemon-reload; \
		echo "Service file: /etc/systemd/system/llm-retry-proxy.service"; \
		echo "Config file:  /etc/llm-retry-proxy/config.toml"; \
		echo ""; \
		echo "Enable and start with:"; \
		echo "  sudo systemctl enable --now llm-retry-proxy"; \
		echo "  sudo systemctl status llm-retry-proxy"; \
	elif [ "$(SERVICE_MANAGER)" = "launchd" ]; then \
		echo "Installing system-wide launchd service..."; \
		sudo mkdir -p /etc/llm-retry-proxy /var/log/llm-retry-proxy; \
		sudo cp config.example.toml /etc/llm-retry-proxy/config.toml; \
		sed -e 's|__BIN_PATH__|$(BIN_PATH)|' \
		    -e 's|__CONFIG_PATH__|/etc/llm-retry-proxy/config.toml|' \
		    -e 's|__LOG_PATH__|/var/log/llm-retry-proxy|' \
		    services/llm-retry-proxy.plist > /tmp/llm-retry-proxy.plist; \
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
	@if [ "$(SERVICE_MANAGER)" = "systemd" ]; then \
		echo "Installing user-level systemd service..."; \
		CONFIG_DIR="$(HOME)/.config/llm-retry-proxy"; \
		SERVICE_DIR="$(HOME)/.config/systemd/user"; \
		mkdir -p "$$CONFIG_DIR" "$$SERVICE_DIR"; \
		cp config.example.toml "$$CONFIG_DIR/config.toml"; \
		CONFIG_PATH="$$CONFIG_DIR/config.toml"; \
		sed -e "s|__BIN_PATH__|$(BIN_PATH)|" \
		    -e "s|__CONFIG_PATH__|$$CONFIG_PATH|" \
		    -e "s|multi-user.target|default.target|" \
		    services/llm-retry-proxy.service > "$$SERVICE_DIR/llm-retry-proxy.service"; \
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
	elif [ "$(SERVICE_MANAGER)" = "launchd" ]; then \
		echo "Installing user-level launchd service..."; \
		CONFIG_DIR="$(HOME)/.config/llm-retry-proxy"; \
		AGENTS_DIR="$(HOME)/Library/LaunchAgents"; \
		mkdir -p "$$CONFIG_DIR" "$$AGENTS_DIR"; \
		cp config.example.toml "$$CONFIG_DIR/config.toml"; \
		CONFIG_PATH="$$CONFIG_DIR/config.toml"; \
		sed -e "s|__BIN_PATH__|$(BIN_PATH)|" \
		    -e "s|__CONFIG_PATH__|$$CONFIG_PATH|" \
		    -e "s|__LOG_PATH__|$$CONFIG_DIR|" \
		    services/llm-retry-proxy.plist > "$$AGENTS_DIR/llm-retry-proxy.plist"; \
		echo "Service file: $$AGENTS_DIR/llm-retry-proxy.plist"; \
		echo "Config file:  $$CONFIG_PATH"; \
		echo ""; \
		echo "Load with:"; \
		echo "  launchctl load $$AGENTS_DIR/llm-retry-proxy.plist"; \
		echo ""; \
		echo "Unload with:"; \
		echo "  launchctl unload $$AGENTS_DIR/llm-retry-proxy.plist"; \
	else \
		echo "No supported service manager found. Skipping."; \
	fi

docker:
	docker build -t $(BINARY) .

clean:
	cargo clean
