CARGO ?= cargo
BIN := $(or $(CARGO_HOME),$(HOME)/.cargo)/bin/quick_type
SIGN_ID := com.quicktype.app
# "-" = ad-hoc; per una firma con certificato: make install SIGN_IDENTITY="<nome certificato>"
SIGN_IDENTITY ?= -

# Il requisito di default (cdhash) cambia a ogni build e invalida il permesso
# Accessibilità: con firma ad-hoc lo si rende stabile fissando l'identifier.
ifeq ($(SIGN_IDENTITY),-)
SIGN_REQ := -r='designated => identifier "$(SIGN_ID)"'
endif

.PHONY: build test install sign check clean

# Build ottimizzata (usa [profile.release] in Cargo.toml)
build:
	$(CARGO) build --release

# Esecuzione dei test
test:
	$(CARGO) test

# Installazione nella folder bin di cargo (~/.cargo/bin), poi firma stabile
install:
	$(CARGO) install --path .
	@$(MAKE) --no-print-directory sign

# Firma il binario installato con identifier stabile (vedi SIGN_REQ)
sign:
	codesign --force --sign "$(SIGN_IDENTITY)" --identifier $(SIGN_ID) $(SIGN_REQ) "$(BIN)"

# Verifica rapida senza codegen
check:
	$(CARGO) check

# Pulizia degli artefatti
clean:
	$(CARGO) clean
