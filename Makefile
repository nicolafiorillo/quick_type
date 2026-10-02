CARGO ?= cargo
LABEL := com.quicktype.app
PLIST := $(HOME)/Library/LaunchAgents/$(LABEL).plist

.PHONY: build test install check clean autostart autostart-remove

# Build ottimizzata (usa [profile.release] in Cargo.toml)
build:
	$(CARGO) build --release

# Esecuzione dei test
test:
	$(CARGO) test

# Installazione nella folder bin di cargo (~/.cargo/bin)
install:
	$(CARGO) install --path .

# Verifica rapida senza codegen
check:
	$(CARGO) check

# Pulizia degli artefatti
clean:
	$(CARGO) clean

# Avvio automatico al login (LaunchAgent macOS): richiede `make install` prima
autostart: install
	@mkdir -p "$(HOME)/Library/LaunchAgents"
	@printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'	<key>Label</key>' \
		'	<string>$(LABEL)</string>' \
		'	<key>ProgramArguments</key>' \
		'	<array>' \
		'		<string>$(HOME)/.cargo/bin/quick_type</string>' \
		'	</array>' \
		'	<key>RunAtLoad</key>' \
		'	<true/>' \
		'	<key>StandardOutPath</key>' \
		'	<string>/tmp/quick_type.log</string>' \
		'	<key>StandardErrorPath</key>' \
		'	<string>/tmp/quick_type.err</string>' \
		'</dict>' \
		'</plist>' > "$(PLIST)"
	-launchctl bootout gui/$(shell id -u) "$(PLIST)" 2>/dev/null
	launchctl bootstrap gui/$(shell id -u) "$(PLIST)"
	@echo "Autostart abilitato: $(PLIST) (log: /tmp/quick_type.log)"

# Disattiva l'avvio automatico e rimuove il LaunchAgent
autostart-remove:
	-launchctl bootout gui/$(shell id -u) "$(PLIST)" 2>/dev/null
	-rm -f "$(PLIST)"
	@echo "Autostart disabilitato"
