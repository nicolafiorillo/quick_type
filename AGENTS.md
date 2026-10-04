# Quick Accent (`quick_type`)

Utility macOS da barra dei menu in Rust: tenendo premuta una vocale (A/E/I/O/U) e premendo Spazio, sostituisce i caratteri base digitati con la vocale accentata (à/è/ì/ò/ù), emulando Quick Accent di PowerToys.

L'intera applicazione vive in [src/main.rs](src/main.rs) (single-file per scelta: niente moduli per un binario così piccolo).

## Comandi

- `cargo check` — verifica rapida (usare dopo ogni modifica)
- `cargo build` / `cargo run` — build di debug
- `cargo build --release` — build ottimizzata (profilo `[profile.release]` in [Cargo.toml](Cargo.toml): `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`; non rimuoverlo, è pensato per un binario menu-bar piccolo)
- `make build` / `make test` / `make install` — wrapper dei comandi cargo (`install` copia il binario in `~/.cargo/bin` e lo firma con `make sign`: firma ad-hoc con requisito `identifier "com.quicktype.app"`, così il permesso Accessibilità non si invalida a ogni reinstallazione; con `SIGN_IDENTITY="<certificato>"` si usa una firma vera). Il requisito basato solo sull'identifier va bene per un tool personale, ma qualunque binario ad-hoc con quell'identifier ne erediterebbe il permesso.
- Avvio automatico al login: voce di menu **"Launch at login"** che (de)registra il LaunchAgent `~/Library/LaunchAgents/com.quicktype.app.plist` a runtime (label `com.quicktype.app`, SSOT: `LAUNCH_AGENT_LABEL`). Log su `/tmp/quick_type.{log,err}` (SSOT: `LOG_PATH_PREFIX`). **Niente `KeepAlive`**: se l'app crasha o viene chiusa dal menu resta chiusa fino al prossimo login (decisione voluta, non riaggiungerlo). Il plist punta a `current_exe()`: attivarlo dal binario installato, non da `target/debug`. **Niente `launchctl` (né `bootstrap` né `bootout`)**: con `RunAtLoad` il bootstrap lancerebbe subito una seconda istanza (bug già incontrato), il bootout fermerebbe quella corrente; il plist da solo basta, launchd lo carica al prossimo login.
- **Runtime**: l'app richiede il permesso di Accessibilità macOS (Impostazioni di Sistema → Privacy e Sicurezza → Accessibilità). Da terminale vale quello del terminale; lanciata da launchd (autostart) il permesso va concesso al binario `~/.cargo/bin/quick_type` stesso, altrimenti `rdev::grab` fallisce con `EventTapError` (l'icona compare comunque). Il test a runtime va fatto dall'utente.
- **Permesso Accessibilità**: all'avvio `accessibility_trusted(true)` mostra il prompt di sistema (aggiunge il binario alla lista); `start_keyboard_grab` ritenta il grab ogni `PERMISSION_RETRY_INTERVAL` finché riesce (errore loggato una sola volta); se manca, il menu mostra la voce disabilitata "Accessibility permission required", che un `NSTimer` nasconde appena il permesso arriva. Non tornare a un grab singolo senza retry.

## Architettura

- `rdev::grab` (feature `unstable_grab`, dipendenza **git** dal branch `main` di Narsil/rdev) intercetta la tastiera a livello HID su un thread dedicato; la `callback` decide per ogni evento se lasciarlo passare (`Some`) o mangiarlo (`None`).
- Stato globale in `static STATE: Mutex<State>`: vocale tenuta premuta (`Held`: tasto, caratteri base digitati, istante, maiuscola) e vocale soppressa post-iniezione. La maiuscola si ricava da `event.name` (rdev lo calcola con Shift e CapsLock).
- Al trigger (Spazio dopo hold, `Option::take_if` su `Held`): `thread::spawn` dorme `INJECT_DELAY` (15 ms), manda N Backspace via `rdev::simulate`, poi inietta il carattere Unicode (maiuscolo se `Held::upper`).
- **Vincolo critico**: `inject_unicode` posta il `CGEvent` a `CGEventTapLocation::SessionEventTap`, **mai** `HIDEventTap`: gli eventi HID verrebbero ri-intercettati dal nostro stesso grab e il keycode 0 verrebbe letto come 'a', corrompendo lo stato. Non cambiare il tap location.
- UI: `NSStatusItem` con policy `Accessory` (no Dock), menu con voce info disabilitata e "Esci" (`sel!(terminate:)` risale la responder chain). `app.run()` è bloccante sul thread principale.

## Convenzioni di progetto

Il progetto segue rigorosamente **DRY**, **SSOT**, **no boilerplate**, **performance**. Ogni modifica deve rispettarli:

- `accent_for(key) -> Option<&'static str>` è l'**unica fonte di verità** per l'insieme delle vocali e la mappa vocale→accento. Aggiungere/togliere una vocale = una riga qui. Non reintrodurre funzioni separate tipo `is_vowel` né duplicare l'elenco nei `match`.
- `APP_NAME` è l'unica fonte di verità per il nome mostrato all'utente (tooltip, menu, stdout).
- Costanti documentate per ogni numero magico (`HOLD_THRESHOLD`, `INJECT_DELAY`); niente literal sparsi.
- **Stringhe utente in inglese britannico** (tooltip, menu, stdout/stderr); i commenti del codice restano in italiano e spiegano il *perché* (vincoli, invarianti), non il *cosa*.

## Approccio moderno

Preferire sempre le soluzioni idiomatiche e aggiornate dell'ecosistema:

- **Rust edition 2024** (`rust-version = "1.88"` in Cargo.toml: servono le let-chains): inline format args (`format!("{APP_NAME} ...")`), let-chains, `Option::take_if` e combinatori (`is_some_and`, `and_then`) invece di flag booleani e `unwrap()`, raw string `r#"..."#` per i testi multilinea (plist), `env::home_dir()`, `for key_down in [true, false]` invece di codice duplicato. Formattazione: `cargo fmt` (style edition 2024).
- **Solo stack `objc2`**: `objc2` / `objc2-foundation` / `objc2-app-kit` / `objc2-core-graphics` / `objc2-core-foundation` / `objc2-application-services` (binding moderni e mantenuti, già usati anche da `rdev` o già nell'albero). **Non** usare i crate legacy `objc`/`cocoa`/`core-graphics`/`libc` per ciò che std o `objc2-*` coprono. Le feature di `objc2-core-graphics` sono elencate esplicitamente (`default-features = false`).
- Preferire API type-safe e zero-cost; niente dipendenze nuove senza una giustificazione concreta (ogni crate appesantisce il binario menu-bar).

## Decisioni già valutate: NON rifattorizzare

Queste alternative sono state analizzate e scartate; non riproporle:

1. `Mutex` + `unwrap()` resta: `parking_lot` non si giustifica per un lock conteso pochissimo.
2. `thread::spawn` per trigger resta: i trigger sono rari e il thread vive ~15-30 ms; un thread pool sarebbe over-engineering.
3. `statusItemWithLength(-1.0)` resta: è `NSVariableStatusItemLength`, già documentato dal commento inline.
4. `let _ = simulate(...)` resta: non esiste un recovery sensato se la simulazione fallisce.
5. `SMAppService` (API moderna macOS 13+) scartato: richiede un .app bundle, mentre quick_type è un binario raw. Si usa il LaunchAgent plist gestito a runtime dalla voce di menu. Non riproporlo finché il binario resta raw.

## Pitfall noti

- `ns_string!` accetta **solo letterali**: per stringhe costruite a runtime (es. con `APP_NAME`) serve `NSString::from_str(&format!(...))`, e `setTitle`/`setToolTip` vogliono `&NSString` (non `&String` — errore E0308 già incontrato).
- `CGEvent::keyboard_set_unicode_string` vuole lunghezza e puntatore **UTF-16** (`str::encode_utf16`), non una `&str`; `new_keyboard_event`/`post` prendono `Option<&CGEvent>`, quindi `Some(&*event)` da un `CFRetained`.
- `NSMenuItem::setTarget` è `unsafe` e la proprietà target è **weak**: l'handler (`MenuHandler`) resta vivo perché `app.run()` non ritorna mai — non spostarlo in uno scope che termina.
- L'ordine dei bracci nel `match` di `callback` conta: `KeyPress(Space)` deve precedere il braccio generico sulle vocali.
- `rdev` è una dipendenza git: un `cargo update` può cambiarne il comportamento; verificare con `cargo check` dopo ogni update.
