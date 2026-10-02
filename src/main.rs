use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2::sel;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar,
};
use objc2_foundation::{ns_string, NSString};
use rdev::{grab, simulate, Event, EventType, Key};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Per quanto tempo va tenuta premuta la vocale prima che la pressione della
/// barra spaziatrice attivi la sostituzione (come il delay di PowerToys).
/// Senza questa soglia, una normale battitura di "e " verrebbe alterata.
const HOLD_THRESHOLD: Duration = Duration::from_millis(200);

/// Nome mostrato all'utente (tooltip, menu, stdout): unica fonte di verità.
const APP_NAME: &str = "Quick Accent";

/// Piccola pausa prima dell'invio dei Backspace, per lasciare che il sistema
/// smaltisca gli eventi di tastiera ancora in coda.
const INJECT_DELAY: Duration = Duration::from_millis(15);

struct State {
    /// Vocale attualmente premuta: (tasto, caratteri base digitati, istante della prima pressione).
    current: Option<(Key, usize, Instant)>,
    /// Vocale il cui auto-repeat va soppresso fino al rilascio (dopo l'iniezione).
    suppressed: Option<Key>,
}

static STATE: Mutex<State> = Mutex::new(State {
    current: None,
    suppressed: None,
});

/// Mappa ogni vocale supportata al suo carattere accentato: unica fonte di
/// verità (SSOT) per l'insieme delle vocali gestite.
fn accent_for(key: Key) -> Option<&'static str> {
    match key {
        Key::KeyA => Some("à"),
        Key::KeyE => Some("è"), // Cambia in "é" se preferisci l'accento acuto
        Key::KeyI => Some("ì"),
        Key::KeyO => Some("ò"),
        Key::KeyU => Some("ù"),
        _ => None,
    }
}

/// Simula la pressione di Backspace (cancella un carattere base già digitato).
fn send_backspace() {
    let _ = simulate(&EventType::KeyPress(Key::Backspace));
    let _ = simulate(&EventType::KeyRelease(Key::Backspace));
}

/// Inietta un carattere Unicode scavalcando il layout di tastiera.
fn inject_unicode(text: &str) {
    if let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) {
        // IMPORTANTE: postiamo a livello Session e non HID. Gli eventi postati a
        // livello HID verrebbero ri-intercettati dal nostro stesso grab (rdev usa
        // un tap HID), e il keycode 0 verrebbe interpretato come 'a', sporcano
        // lo stato interno.
        for key_down in [true, false] {
            if let Ok(event) = CGEvent::new_keyboard_event(source.clone(), 0, key_down) {
                event.set_string(text);
                event.post(CGEventTapLocation::Session);
            }
        }
    }
}

fn callback(event: Event) -> Option<Event> {
    match event.event_type {
        // La barra spaziatrice va gestita PRIMA del braccio generico sulle
        // pressioni di tasto, altrimenti verrebbe catturata da quest'ultimo e
        // questo braccio non verrebbe mai eseguito.
        EventType::KeyPress(Key::Space) => {
            let mut state = STATE.lock().unwrap();
            let triggered = match &state.current {
                Some((_, _, pressed_at)) => pressed_at.elapsed() >= HOLD_THRESHOLD,
                None => false,
            };
            if triggered {
                let (vowel, count, _) = state.current.take().unwrap();
                state.suppressed = Some(vowel);
                thread::spawn(move || {
                    thread::sleep(INJECT_DELAY);
                    // Tenendo premuto il tasto, l'auto-repeat può aver digitato
                    // più copie della vocale: le cancelliamo tutte.
                    for _ in 0..count {
                        send_backspace();
                    }
                    if let Some(accent) = accent_for(vowel) {
                        inject_unicode(accent);
                    }
                });
                // Mangiamo lo spazio: non deve comparire a schermo.
                return None;
            }
            Some(event)
        }
        // Pressione di una vocale: tracciamo quale è premuta, da quando, e
        // quanti caratteri base ha digitato (l'auto-repeat genera pressioni
        // ripetute, una per carattere).
        EventType::KeyPress(key) if accent_for(key).is_some() => {
            let mut state = STATE.lock().unwrap();
            if state.suppressed == Some(key) {
                // Auto-repeat residuo dopo l'iniezione: lo mangiamo.
                return None;
            }
            match &mut state.current {
                Some((k, n, _)) if *k == key => *n += 1,
                current => *current = Some((key, 1, Instant::now())),
            }
            Some(event)
        }
        // Rilascio di una vocale: resettiamo lo stato.
        EventType::KeyRelease(key) if accent_for(key).is_some() => {
            let mut state = STATE.lock().unwrap();
            if let Some((k, ..)) = &state.current {
                if *k == key {
                    state.current = None;
                }
            }
            if state.suppressed == Some(key) {
                state.suppressed = None;
            }
            Some(event)
        }
        // Lasciamo passare tutti gli altri tasti normalmente.
        _ => Some(event),
    }
}

/// Avvia il listener della tastiera (bloccante) su un thread in background.
fn start_keyboard_grab() {
    thread::spawn(|| {
        if let Err(error) = grab(callback) {
            eprintln!("Critical error while grabbing the keyboard: {error:?}");
            eprintln!("Make sure the terminal has Accessibility permission in System Settings.");
        }
    });
}

/// Crea l'icona nella barra dei menu di macOS con un menu di contesto.
/// Deve essere chiamata sul thread principale.
fn setup_status_item(mtm: MainThreadMarker) {
    // Policy "Accessory": nessuna icona nel Dock, app solo nella barra dei menu.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Creiamo lo status item con un'etichetta testuale (una "à" stilizzata).
    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0); // -1.0 = NSVariableStatusItemLength
    if let Some(button) = item.button(mtm) {
        button.setTitle(ns_string!("à"));
        button.setToolTip(Some(ns_string!(
            "Quick Accent active: hold a vowel and press Space"
        )));
    }

    // Menu a tendina: voce informativa (disabilitata) e "Esci".
    let menu = NSMenu::new(mtm);
    let info = NSMenuItem::new(mtm);
    // env!("CARGO_PKG_VERSION") legge la versione da Cargo.toml a compile time (SSOT).
    let info_title = NSString::from_str(&format!("{APP_NAME} v{}", env!("CARGO_PKG_VERSION")));
    info.setTitle(&info_title);
    info.setEnabled(false);
    menu.addItem(&info);

    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            ns_string!("Quit"),
            // Nessun target esplicito: "terminate:" risale la responder chain
            // fino a NSApplication, che chiude l'app.
            Some(sel!(terminate:)),
            ns_string!("q"),
        )
    };
    menu.addItem(&quit);
    item.setMenu(Some(&menu));

    // Avvia il run loop di AppKit (bloccante). Le variabili locali restano
    // vive per tutta la durata del processo, mantenendo attivo lo status item.
    app.run();
}

fn main() {
    println!("Mac {APP_NAME} (Universal Unicode Mode) - Started!");
    println!("Hold a vowel (A, E, I, O, U) and press Space.");
    println!("Press Ctrl+C or use the menu bar item to quit.");

    start_keyboard_grab();

    let mtm = MainThreadMarker::new().expect("main deve girare sul thread principale");
    setup_status_item(mtm);
}