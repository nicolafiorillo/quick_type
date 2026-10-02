CARGO ?= cargo

.PHONY: build test install check clean

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
