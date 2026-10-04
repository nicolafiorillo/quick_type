use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton,
    NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem, NSStatusBar, NSTextField,
    NSView, NSWindow, NSWindowStyleMask,
};
use objc2_application_services::{
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer, NSUserDefaults, ns_string};
use rdev::{Event, EventType, Key, grab, set_is_main_thread, simulate};

/// Per quanto tempo va tenuta premuta la vocale prima che la pressione della
/// barra spaziatrice attivi la sostituzione (come il delay di PowerToys).
/// Senza questa soglia, una normale battitura di "e " verrebbe alterata.
const DEFAULT_HOLD_THRESHOLD_MS: u64 = 300;
const HOLD_THRESHOLD_KEY: &str = "holdThresholdMs";

static HOLD_THRESHOLD_MS: AtomicU64 = AtomicU64::new(DEFAULT_HOLD_THRESHOLD_MS);

struct SettingsWindow {
    window: Retained<NSWindow>,
    launch_at_login: Retained<NSButton>,
    hold_threshold: Retained<NSTextField>,
}

thread_local! {
    static SETTINGS_WINDOW: RefCell<Option<SettingsWindow>> = const { RefCell::new(None) };
}

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

fn load_hold_threshold_ms() -> u64 {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(HOLD_THRESHOLD_KEY);
    if defaults.objectForKey(&key).is_some() {
        u64::try_from(defaults.integerForKey(&key)).unwrap_or(DEFAULT_HOLD_THRESHOLD_MS)
    } else {
        DEFAULT_HOLD_THRESHOLD_MS
    }
}

fn save_hold_threshold_ms(value: u64) -> bool {
    let Ok(value) = isize::try_from(value) else {
        return false;
    };
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(HOLD_THRESHOLD_KEY);
    defaults.setInteger_forKey(value, &key);
    HOLD_THRESHOLD_MS.store(value as u64, Ordering::Relaxed);
    true
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
            let Some(held) = state.current.take_if(|held| {
                held.since.elapsed()
                    >= Duration::from_millis(HOLD_THRESHOLD_MS.load(Ordering::Relaxed))
            }) else {
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
        // Il tap gira in background: rdev deve tradurre i keycode sul main thread AppKit.
        set_is_main_thread(false);
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

        #[unsafe(method(applySettings:))]
        fn apply_settings(&self, _sender: &NSButton) {
            SETTINGS_WINDOW.with(|stored| {
                let stored = stored.borrow();
                let Some(settings) = stored.as_ref() else {
                    return;
                };
                let value = settings.hold_threshold.stringValue().to_string().parse::<u64>();
                let Ok(value) = value else {
                    settings.hold_threshold.setStringValue(&NSString::from_str(
                        &HOLD_THRESHOLD_MS.load(Ordering::Relaxed).to_string(),
                    ));
                    return;
                };
                if !save_hold_threshold_ms(value) {
                    return;
                }
                set_autostart(settings.launch_at_login.state() == NSControlStateValueOn);
                settings.window.close();
            });
        }

        #[unsafe(method(cancelSettings:))]
        fn cancel_settings(&self, _sender: &NSButton) {
            SETTINGS_WINDOW.with(|stored| {
                if let Some(settings) = stored.borrow().as_ref() {
                    settings.window.close();
                }
            });
        }

        #[unsafe(method(showSettings:))]
        fn show_settings(&self, _sender: &NSMenuItem) {
            let mtm = MainThreadMarker::new().expect("settings deve girare sul thread principale");
            show_settings_window(mtm, self);
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
            ns_string!("Settings..."),
            Some(sel!(showSettings:)),
            ns_string!(""),
        )
    };
    unsafe { autostart.setTarget(Some(&handler)) };
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

fn show_settings_window(mtm: MainThreadMarker, handler: &MenuHandler) {
    NSApplication::sharedApplication(mtm).activate();
    SETTINGS_WINDOW.with(|stored| {
        if let Some(settings) = stored.borrow().as_ref() {
            if !settings.window.isVisible() {
                settings.launch_at_login.setState(if autostart_enabled() {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                settings.hold_threshold.setStringValue(&NSString::from_str(
                    &HOLD_THRESHOLD_MS.load(Ordering::Relaxed).to_string(),
                ));
            }
            settings.window.makeKeyAndOrderFront(None);
            return;
        }
        let settings = create_settings_window(mtm, handler);
        settings.window.center();
        settings.window.makeKeyAndOrderFront(None);
        *stored.borrow_mut() = Some(settings);
    });
}

fn create_settings_window(mtm: MainThreadMarker, handler: &MenuHandler) -> SettingsWindow {
    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(430.0, 210.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            content_rect,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Settings"));

    let content = NSView::initWithFrame(mtm.alloc(), content_rect);
    let launch_at_login = unsafe {
        NSButton::checkboxWithTitle_target_action(ns_string!("Launch at login"), None, None, mtm)
    };
    launch_at_login.setState(if autostart_enabled() {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    launch_at_login.setFrame(NSRect::new(
        NSPoint::new(20.0, 155.0),
        NSSize::new(300.0, 24.0),
    ));
    content.addSubview(&launch_at_login);

    let threshold_label = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(20.0, 105.0), NSSize::new(255.0, 24.0)),
    );
    threshold_label.setStringValue(ns_string!("Hold threshold"));
    threshold_label.setEditable(false);
    threshold_label.setBordered(false);
    threshold_label.setDrawsBackground(false);
    content.addSubview(&threshold_label);

    let threshold_field = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(285.0, 105.0), NSSize::new(80.0, 24.0)),
    );
    threshold_field.setStringValue(&NSString::from_str(
        &HOLD_THRESHOLD_MS.load(Ordering::Relaxed).to_string(),
    ));
    content.addSubview(&threshold_field);

    let unit_label = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(375.0, 105.0), NSSize::new(40.0, 24.0)),
    );
    unit_label.setStringValue(ns_string!("ms"));
    unit_label.setEditable(false);
    unit_label.setBordered(false);
    unit_label.setDrawsBackground(false);
    content.addSubview(&unit_label);

    let cancel = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(250.0, 20.0), NSSize::new(80.0, 30.0)),
    );
    cancel.setTitle(ns_string!("Cancel"));
    unsafe { cancel.setTarget(Some(handler)) };
    unsafe { cancel.setAction(Some(sel!(cancelSettings:))) };
    content.addSubview(&cancel);

    let ok = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(340.0, 20.0), NSSize::new(80.0, 30.0)),
    );
    ok.setTitle(ns_string!("OK"));
    unsafe { ok.setTarget(Some(handler)) };
    unsafe { ok.setAction(Some(sel!(applySettings:))) };
    content.addSubview(&ok);

    window.setContentView(Some(&content));
    SettingsWindow {
        window,
        launch_at_login,
        hold_threshold: threshold_field,
    }
}

fn main() {
    println!("Mac {APP_NAME} (Universal Unicode Mode) - Started!");
    println!("Hold a vowel (A, E, I, O, U) and press Space.");
    println!("Press Ctrl+C or use the menu bar item to quit.");

    // Il prompt di sistema aggiunge il binario alla lista Accessibilità: senza
    // di esso, lanciato da launchd, l'app non comparirebbe tra quelle abilitabili.
    let trusted = accessibility_trusted(true);
    HOLD_THRESHOLD_MS.store(load_hold_threshold_ms(), Ordering::Relaxed);
    start_keyboard_grab();

    let mtm = MainThreadMarker::new().expect("main deve girare sul thread principale");
    run_status_item(mtm, trusted);
}
