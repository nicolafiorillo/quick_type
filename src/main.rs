use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSMenu, NSMenuItem, NSStatusBar,
};
use objc2_application_services::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
use objc2_foundation::{NSString, NSTimer, ns_string};
use rdev::{Event, EventType, Key, grab, simulate};

/// Per quanto tempo va tenuta premuta la vocale prima che la pressione della
/// barra spaziatrice attivi la sostituzione (come il delay di PowerToys).
/// Senza questa soglia, una normale battitura di "e " verrebbe alterata.
const HOLD_THRESHOLD: Duration = Duration::from_millis(100);

/// Piccola pausa prima dell'invio dei Backspace, per lasciare che il sistema
/// smaltisca gli eventi di tastiera ancora in coda.
const INJECT_DELAY: Duration = Duration::from_millis(15);

/// Ogni quanto si ritenta il grab della tastiera (e si ricontrolla il permesso
/// nel menu) finché l'Accessibilità non viene concessa.
const PERMISSION_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Nome mostrato all'utente (tooltip, menu, stdout): unica fonte di verità.
const APP_NAME: &str = "Quick Accent";

/// Label del LaunchAgent per l'avvio automatico al login: unica fonte di verità.
const LAUNCH_AGENT_LABEL: &str = "com.quicktype.app";

/// Prefisso dei file di log del LaunchAgent (`.log` per stdout, `.err` per stderr).
const LOG_PATH_PREFIX: &str = "/tmp/quick_type";

/// Vocale attualmente tenuta premuta.
struct Held {
    key: Key,
    /// Caratteri base già digitati: l'auto-repeat ne genera uno per pressione.
    count: usize,
    since: Instant,
    /// Digitata con Shift o CapsLock: l'accento va iniettato maiuscolo.
    upper: bool,
}

struct State {
    current: Option<Held>,
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

/// Path del plist del LaunchAgent: ~/Library/LaunchAgents/<label>.plist.
fn launch_agent_path() -> Option<PathBuf> {
    Some(env::home_dir()?.join(format!("Library/LaunchAgents/{LAUNCH_AGENT_LABEL}.plist")))
}

/// L'avvio automatico è attivo se il plist esiste.
fn autostart_enabled() -> bool {
    launch_agent_path().is_some_and(|plist| plist.exists())
}

/// (Dis)attiva l'avvio automatico scrivendo/rimuovendo il plist del LaunchAgent.
/// Niente KeepAlive (se l'app viene chiusa resta chiusa) e niente
/// `launchctl bootstrap/bootout`: con RunAtLoad il bootstrap avvierebbe subito
/// una seconda istanza, il bootout fermerebbe quella corrente. launchd carica
/// il plist da solo al prossimo login.
fn set_autostart(enabled: bool) {
    let Some(plist) = launch_agent_path() else {
        return;
    };
    if !enabled {
        let _ = fs::remove_file(plist);
        return;
    }
    let Ok(exe) = env::current_exe() else { return };
    let exe = exe.display();
    let contents = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{LOG_PATH_PREFIX}.log</string>
	<key>StandardErrorPath</key>
	<string>{LOG_PATH_PREFIX}.err</string>
</dict>
</plist>
"#
    );
    let _ = fs::write(plist, contents);
}

/// Simula la pressione di Backspace (cancella un carattere base già digitato).
fn send_backspace() {
    let _ = simulate(&EventType::KeyPress(Key::Backspace));
    let _ = simulate(&EventType::KeyRelease(Key::Backspace));
}

/// Inietta un carattere Unicode scavalcando il layout di tastiera.
fn inject_unicode(text: &str) {
    let Some(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return;
    };
    let utf16: Vec<u16> = text.encode_utf16().collect();
    for key_down in [true, false] {
        if let Some(event) = CGEvent::new_keyboard_event(Some(&source), 0, key_down) {
            // SAFETY: il puntatore e la lunghezza descrivono `utf16`, vivo per tutta la chiamata.
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    Some(&event),
                    utf16.len() as _,
                    utf16.as_ptr(),
                );
            }
            // IMPORTANTE: Session e non HID. Gli eventi HID verrebbero ri-intercettati
            // dal nostro stesso grab (rdev usa un tap HID) e il keycode 0 letto come 'a',
            // sporcando lo stato interno.
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
        }
    }
}

fn callback(event: Event) -> Option<Event> {
    match event.event_type {
        // Va gestita PRIMA del braccio generico sulle pressioni di tasto, che
        // altrimenti la catturerebbe.
        EventType::KeyPress(Key::Space) => {
            let mut state = STATE.lock().unwrap();
            let Some(held) = state
                .current
                .take_if(|held| held.since.elapsed() >= HOLD_THRESHOLD)
            else {
                return Some(event);
            };
            state.suppressed = Some(held.key);
            thread::spawn(move || {
                thread::sleep(INJECT_DELAY);
                // L'auto-repeat può aver digitato più copie della vocale: le cancelliamo tutte.
                for _ in 0..held.count {
                    send_backspace();
                }
                let Some(accent) = accent_for(held.key) else {
                    return;
                };
                inject_unicode(&if held.upper {
                    accent.to_uppercase()
                } else {
                    accent.to_owned()
                });
            });
            // Mangiamo lo spazio: non deve comparire a schermo.
            None
        }
        // Pressione di una vocale: tracciamo quale è premuta e da quando.
        EventType::KeyPress(key) if accent_for(key).is_some() => {
            // event.name riflette Shift e CapsLock ("A" vs "a").
            let upper = event
                .name
                .as_deref()
                .and_then(|name| name.chars().next())
                .is_some_and(char::is_uppercase);
            let mut state = STATE.lock().unwrap();
            if state.suppressed == Some(key) {
                // Auto-repeat residuo dopo l'iniezione: lo mangiamo.
                return None;
            }
            match &mut state.current {
                Some(held) if held.key == key => held.count += 1,
                current => {
                    *current = Some(Held {
                        key,
                        count: 1,
                        since: Instant::now(),
                        upper,
                    });
                }
            }
            Some(event)
        }
        // Rilascio di una vocale: resettiamo lo stato.
        EventType::KeyRelease(key) if accent_for(key).is_some() => {
            let mut state = STATE.lock().unwrap();
            state.current.take_if(|held| held.key == key);
            if state.suppressed == Some(key) {
                state.suppressed = None;
            }
            Some(event)
        }
        _ => Some(event),
    }
}

/// Avvia il listener della tastiera su un thread in background. Senza permesso
/// Accessibilità (tipico all'avvio al login) il grab fallisce: si ritenta finché
/// il permesso non viene concesso.
fn start_keyboard_grab() {
    thread::spawn(|| {
        let mut reported = false;
        while let Err(error) = grab(callback) {
            if !reported {
                eprintln!(
                    "Keyboard grab failed ({error:?}): grant Accessibility permission to this binary in System Settings; retrying."
                );
                reported = true;
            }
            thread::sleep(PERMISSION_RETRY_INTERVAL);
        }
    });
}

/// Se il processo ha il permesso Accessibilità; con `prompt` mostra anche la
/// richiesta di sistema, che aggiunge il binario alla lista in Impostazioni.
fn accessibility_trusted(prompt: bool) -> bool {
    if !prompt {
        return unsafe { AXIsProcessTrusted() };
    }
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(
        &[unsafe { kAXTrustedCheckOptionPrompt }],
        &[CFBoolean::new(true)],
    );
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_ref())) }
}

// Handler delle azioni di menu: main-thread-only, senza stato (l'autostart si
// legge dal filesystem a ogni toggle).
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(refreshPermission:))]
        fn refresh_permission(&self, timer: &NSTimer) {
            if !accessibility_trusted(false) {
                return;
            }
            // userInfo è la voce di avviso: concesso il permesso la nascondiamo e ci fermiamo.
            if let Some(warning) = timer
                .userInfo()
                .and_then(|info| info.downcast::<NSMenuItem>().ok())
            {
                warning.setHidden(true);
            }
            timer.invalidate();
        }

        #[unsafe(method(toggleAutostart:))]
        fn toggle_autostart(&self, sender: &NSMenuItem) {
            set_autostart(!autostart_enabled());
            // La spunta riflette lo stato reale dopo l'operazione.
            sender.setState(if autostart_enabled() {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }
);

impl MenuHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![mtm.alloc::<Self>(), init] }
    }
}

/// Crea l'icona nella barra dei menu con il relativo menu, poi avvia il run
/// loop di AppKit (bloccante). Deve essere chiamata sul thread principale.
fn run_status_item(mtm: MainThreadMarker, trusted: bool) {
    // Policy "Accessory": nessuna icona nel Dock, app solo nella barra dei menu.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0); // -1.0 = NSVariableStatusItemLength
    if let Some(button) = item.button(mtm) {
        button.setTitle(ns_string!("è"));
        let tooltip =
            NSString::from_str(&format!("{APP_NAME} active: hold a vowel and press Space"));
        button.setToolTip(Some(&tooltip));
    }

    let menu = NSMenu::new(mtm);

    let info = NSMenuItem::new(mtm);
    // env! legge la versione da Cargo.toml a compile time (SSOT).
    info.setTitle(&NSString::from_str(&format!(
        "{APP_NAME} v{}",
        env!("CARGO_PKG_VERSION")
    )));
    info.setEnabled(false);
    menu.addItem(&info);

    let warning = NSMenuItem::new(mtm);
    warning.setTitle(ns_string!("Accessibility permission required"));
    warning.setEnabled(false);
    warning.setHidden(trusted);
    menu.addItem(&warning);

    let handler = MenuHandler::new(mtm);
    if !trusted {
        // NSTimer trattiene il target; il run loop trattiene il timer.
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                PERMISSION_RETRY_INTERVAL.as_secs_f64(),
                &handler,
                sel!(refreshPermission:),
                Some(&warning),
                true,
            );
        }
    }
    let autostart = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            ns_string!("Launch at login"),
            Some(sel!(toggleAutostart:)),
            ns_string!(""),
        )
    };
    // Il target è weak: `handler` resta vivo perché app.run() non ritorna mai.
    unsafe { autostart.setTarget(Some(&handler)) };
    if autostart_enabled() {
        autostart.setState(NSControlStateValueOn);
    }
    menu.addItem(&autostart);

    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            ns_string!("Quit"),
            // Senza target esplicito "terminate:" risale la responder chain fino a NSApplication.
            Some(sel!(terminate:)),
            ns_string!("q"),
        )
    };
    menu.addItem(&quit);
    item.setMenu(Some(&menu));

    app.run();
}

fn main() {
    println!("Mac {APP_NAME} (Universal Unicode Mode) - Started!");
    println!("Hold a vowel (A, E, I, O, U) and press Space.");
    println!("Press Ctrl+C or use the menu bar item to quit.");

    // Il prompt di sistema aggiunge il binario alla lista Accessibilità: senza
    // di esso, lanciato da launchd, l'app non comparirebbe tra quelle abilitabili.
    let trusted = accessibility_trusted(true);
    start_keyboard_grab();

    let mtm = MainThreadMarker::new().expect("main deve girare sul thread principale");
    run_status_item(mtm, trusted);
}
