use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use rdev::{grab, simulate, Event, EventType, Key};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Per quanto tempo va tenuta premuta la vocale prima che la pressione della
/// barra spaziatrice attivi la sostituzione (come il delay di PowerToys).
/// Senza questa soglia, una normale battitura di "e " verrebbe alterata.
const HOLD_THRESHOLD: Duration = Duration::from_millis(200);

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

fn is_vowel(key: Key) -> bool {
    matches!(key, Key::KeyA | Key::KeyE | Key::KeyI | Key::KeyO | Key::KeyU)
}

fn accent_for(vowel: Key) -> &'static str {
    match vowel {
        Key::KeyA => "à",
        Key::KeyE => "è", // Cambia in "é" se preferisci l'accento acuto
        Key::KeyI => "ì",
        Key::KeyO => "ò",
        Key::KeyU => "ù",
        _ => "",
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
        if let Ok(event_down) = CGEvent::new_keyboard_event(source.clone(), 0, true) {
            event_down.set_string(text);
            event_down.post(CGEventTapLocation::Session);
        }
        if let Ok(event_up) = CGEvent::new_keyboard_event(source, 0, false) {
            event_up.set_string(text);
            event_up.post(CGEventTapLocation::Session);
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

                    let char = accent_for(vowel);
                    inject_unicode(char);

                    print!("Carattere sostituito: {}", char);
                });
                // Mangiamo lo spazio: non deve comparire a schermo.
                return None;
            }
            Some(event)
        }
        // Pressione di una vocale: tracciamo quale è premuta, da quando, e
        // quanti caratteri base ha digitato (l'auto-repeat genera pressioni
        // ripetute, una per carattere).
        EventType::KeyPress(key) if is_vowel(key) => {
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
        EventType::KeyRelease(key) if is_vowel(key) => {
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

fn main() {
    println!("Mac Quick Accent (Modalità Universale Unicode) - Avviato!");
    println!("Tieni premuta una vocale (A, E, I, O, U) e premi Spazio.");
    println!("Premi Ctrl+C per terminare l'applicazione.");

    if let Err(error) = grab(callback) {
        eprintln!("Errore critico durante l'intercettazione: {:?}", error);
        eprintln!("Assicurati che il Terminale abbia i permessi di Accessibilità in Impostazioni di Sistema.");
    }
}