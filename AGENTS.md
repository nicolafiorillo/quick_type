# Quick Accent (`quick_type`)

Utility macOS da barra dei menu in Rust: tenendo premuta una vocale (A/E/I/O/U) e premendo Spazio, sostituisce i caratteri base digitati con la vocale accentata (à/è/ì/ò/ù), emulando Quick Accent di PowerToys.

L'intera applicazione vive in [src/main.rs](src/main.rs) (~200 righe, single-file per scelta: niente moduli per un binario così piccolo).

## Comandi

- `cargo check` — verifica rapida (usare dopo ogni modifica)
- `cargo build` / `cargo run` — build di debug
- `cargo build --release` — build ottimizzata (profilo `[profile.release]` in [Cargo.toml](Cargo.toml): `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`; non rimuoverlo, è pensato per un binario menu-bar piccolo)
- `make build` / `make test` / `make install` — wrapper dei comandi cargo (`install` copia il binario in `~/.cargo/bin`)
- Avvio automatico al login: voce di menu **"Launch at login"** che (de)registra il LaunchAgent `~/Library/LaunchAgents/com.quicktype.app.plist` a runtime (label `com.quicktype.app`, SSOT: `LAUNCH_AGENT_LABEL`). Log su `/tmp/quick_type.{log,err}`. **Niente `KeepAlive`**: se l'app crasha o viene chiusa dal menu resta chiusa fino al prossimo login (decisione voluta, non riaggiungerlo). Il plist punta a `current_exe()`: attivarlo dal binario installato, non da `target/debug`. **Niente `launchctl bootstrap` all'attivazione**: con `RunAtLoad` launchd lancerebbe subito una seconda istanza — il plist da solo basta per il prossimo login (bug già incontrato).
- **Runtime**: l'app richiede i permessi di Accessibilità macOS (Impostazioni di Sistema → Privacy e Sicurezza → Accessibilità) per il terminale/l'app che la esegue; senza, `rdev::grab` fallisce. Il test a runtime va fatto dall'utente. Con l'autostart attivo, il permesso va concesso al binario `quick_type` stesso.

## Architettura

- `rdev::grab` (feature `unstable_grab`, dipendenza **git** dal branch `main` di Narsil/rdev) intercetta la tastiera a livello HID su un thread dedicato; la `callback` decide per ogni evento se lasciarlo passare (`Some`) o mangiarlo (`None`).
- Stato globale in `static STATE: Mutex<State>`: vocale corrente `(Key, usize, Instant)` e vocale soppressa post-iniezione.
- Al trigger (Spazio dopo hold): `thread::spawn` dorme `INJECT_DELAY` (15 ms), manda N Backspace via `rdev::simulate`, poi inietta il carattere Unicode.
- **Vincolo critico**: `inject_unicode` posta il `CGEvent` a `CGEventTapLocation::Session`, **mai** HID: gli eventi HID verrebbero ri-intercettati dal nostro stesso grab e il keycode 0 verrebbe letto come 'a', corrompendo lo stato. Non cambiare il tap location.
- UI: `NSStatusItem` con policy `Accessory` (no Dock), menu con voce info disabilitata e "Esci" (`sel!(terminate:)` risale la responder chain). `app.run()` è bloccante sul thread principale.

## Convenzioni di progetto

Il progetto segue rigorosamente **DRY**, **SSOT**, **no boilerplate**, **performance**. Ogni modifica deve rispettarli:

- `accent_for(key) -> Option<&'static str>` è l'**unica fonte di verità** per l'insieme delle vocali e la mappa vocale→accento. Aggiungere/togliere una vocale = una riga qui. Non reintrodurre funzioni separate tipo `is_vowel` né duplicare l'elenco nei `match`.
- `APP_NAME` è l'unica fonte di verità per il nome mostrato all'utente (menu, stdout).
- Costanti documentate per ogni numero magico (`HOLD_THRESHOLD`, `INJECT_DELAY`); niente literal sparsi.
- **Stringhe utente in inglese britannico** (tooltip, menu, stdout/stderr); i commenti del codice restano in italiano e spiegano il *perché* (vincoli, invarianti), non il *cosa*.

## Approccio moderno

Preferire sempre le soluzioni idiomatiche e aggiornate dell'ecosistema:

- **Rust edition 2024**: inline format args (`format!("{APP_NAME} ...")`), match guards, `Option`/combinatori invece di flag booleani, `for key_down in [true, false]` invece di codice duplicato.
- **`objc2` / `objc2-foundation` / `objc2-app-kit`** (binding moderni e mantenuti) — **non** usare i crate legacy `objc`/`cocoa`/`core-graphics` oltre a quanto già presente.
- Preferire API type-safe e zero-cost; niente dipendenze nuove senza una giustificazione concreta (ogni crate appesantisce il binario menu-bar).

## Decisioni già valutate: NON rifattorizzare

Queste alternative sono state analizzate e scartate; non riproporle:

1. `Mutex` + `unwrap()` resta: `parking_lot` non si giustifica per un lock conteso pochissimo.
2. `thread::spawn` per trigger resta: i trigger sono rari e il thread vive ~15-30 ms; un thread pool sarebbe over-engineering.
3. `statusItemWithLength(-1.0)` resta: è `NSVariableStatusItemLength`, già documentato dal commento inline.
4. `let _ = simulate(...)` resta: non esiste un recovery sensato se la simulazione fallisce.
5. `SMAppService` (API moderna macOS 13+) scartato: richiede un .app bundle, mentre quick_type è un binario raw. Si usa il LaunchAgent plist gestito a runtime dalla voce di menu. Non riproporlo finché il binario resta raw.

## Pitfall noti

- `ns_string!` accetta **solo letterali**: per stringhe costruite a runtime serve `NSString::from_str(&format!(...))`, e `setTitle` vuole `&NSString` (non `&String` — errore E0308 già incontrato).
- `NSMenuItem::setTarget` è `unsafe` e la proprietà target è **weak**: l'handler (`MenuHandler`) resta vivo perché `app.run()` non ritorna mai — non spostarlo in uno scope che termina.
- L'ordine dei bracci nel `match` di `callback` conta: `KeyPress(Space)` deve precedere il braccio generico sulle vocali.
- `rdev` è una dipendenza git: un `cargo update` può cambiarne il comportamento; verificare con `cargo check` dopo ogni update.
