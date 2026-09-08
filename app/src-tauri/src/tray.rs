//! Menu bar tray icon: unseen count as a title badge, left click opens a
//! popover anchored under the icon (Tray.dc.html), right click shows a menu
//! (Poll now, Open, Quit).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};
use tauri::menu::{Menu, MenuEvent, MenuItem};
use tauri::Emitter;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, PhysicalPosition, Position, Rect};

use crate::engine_api::EngineApi;
use crate::poller;
use crate::EngineState;

pub const TRAY_ID: &str = "main-tray";
pub const POPOVER_LABEL: &str = "tray-popover";

/// How long after the popover is hidden by something other than its own
/// "already open, close it" branch (lib.rs's `WindowEvent::Focused(false)`
/// handler, or `CLICK_OUTSIDE_MONITOR` below) a left click on the tray icon
/// is treated as "that's the very click that just closed it" rather than
/// "the user wants to reopen it." A few tens of ms covers the gap between
/// that hide and this file's own click handler running for the same click;
/// anything a user-perceptible amount of time later is a deliberate second
/// click and should open it again.
const REOPEN_DEBOUNCE: Duration = Duration::from_millis(200);

/// Tracks when the popover last closed itself for a reason other than the
/// tray icon's own re-click (see `toggle_popover`'s "already visible" early
/// return), so that same function can tell the two apart on the very next
/// click. Managed as app state; read here, written from two places:
///
/// - lib.rs's `Focused(false)` handler, for a click that lands on one of
///   *our own* other windows (the main window) — a same-app key-window
///   handoff, which macOS reports reliably.
/// - `CLICK_OUTSIDE_MONITOR`'s handler below, for a click that lands
///   anywhere else entirely (another app, the desktop) — see that monitor's
///   doc comment for why `Focused(false)` alone can't be trusted to cover
///   this case too.
///
/// Why the race this guards against exists: clicking the tray icon while
/// the popover is open both (a) is itself an "outside" click as far as
/// whichever mechanism above is watching, so it can hide the popover on its
/// own — and (b) is the same click this file's `on_tray_icon_event` handler
/// sees, arriving via a separate, slightly later event (its mouse-up half).
/// If (a) is delivered and processed before (b), `toggle_popover` finds the
/// window already hidden and, with no guard, would treat that as "closed,
/// so open it" — undoing the close the user just performed in the same
/// click.
pub struct PopoverFocusState(Mutex<Option<Instant>>);

impl PopoverFocusState {
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Called from lib.rs's `Focused(false)` handler and from
    /// `CLICK_OUTSIDE_MONITOR`'s handler, right after either hides the
    /// popover for a reason other than the tray icon's own re-click.
    pub fn note_external_hide(&self) {
        *self.0.lock().unwrap() = Some(Instant::now());
    }

    fn recently_hidden_externally(&self) -> bool {
        match *self.0.lock().unwrap() {
            Some(t) => t.elapsed() < REOPEN_DEBOUNCE,
            None => false,
        }
    }
}

/// Static tray icon PNG embedded at compile time (no runtime resource
/// resolution needed) — the coloured mark (icons/tray-color.svg) rather
/// than the monochrome template image (icons/tray.png). Only the @2x
/// (44x44) raster is embedded, not the 1x: the macOS tray backend
/// (tray-icon crate) always rescales whatever NSImage it's given to a
/// fixed 18pt logical height, so there's no scale factor to pick between
/// at build time — a single higher-resolution source just gets a cleaner
/// downscale on every display. A template image auto-adapts to the menu
/// bar's light/dark theme and its click-highlight state; a coloured one
/// does not — the green mark was chosen to match the rest of the
/// app, so `icon_as_template(false)` below trades that automatic
/// adaptation away on purpose.
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-color@2x.png");

/// Toggles the tray popover, positioning it centered under the icon's rect
/// (physical pixels) each time it's shown, so it tracks the icon even if
/// the menu bar layout shifts between clicks — then clamped to the work
/// area of the icon's monitor so it stays fully on screen.
///
/// A prior version of this comment reasoned that `window.set_focus()` (tao
/// 0.35.3, `platform_impl/macos/util/async.rs::set_focus`) — which issues
/// `makeKeyAndOrderFront` plus `[NSApp activateIgnoringOtherApps:YES]` —
/// ought to be enough on its own to make lib.rs's `WindowEvent::Focused
/// (false)` handler fire reliably when the user clicks outside. Actually
/// running the app with logging on every step of this path (see this
/// packet's report for the full sequence) showed that reasoning was wrong:
/// across repeated trials, `set_focus()` returning `Ok(())` did not
/// reliably mean the window became key soon after — `is_focused()` sampled
/// right after came back `false` at least as often as `true`, and even
/// when a `Focused(true)`/`Focused(false)` pair *did* show up, its timing
/// was wildly inconsistent (immediate in one trial, ten-plus seconds later
/// in another). Whatever the exact cause, that's not a signal a dismiss
/// behavior can be built on. `install_click_outside_monitor` below is the
/// fix: a raw AppKit global mouse-down monitor that doesn't depend on this
/// window ever becoming key at all. The `Focused(false)` handler stays too
/// (belt and braces, see its own comment in lib.rs) for the one case the
/// monitor can't see — a click on one of *our own* other windows.
fn toggle_popover(app: &AppHandle, icon_rect: Rect) {
    let Some(window) = app.get_webview_window(POPOVER_LABEL) else { return };
    if matches!(window.is_visible(), Ok(true)) {
        remove_click_outside_monitor();
        let _ = window.hide();
        return;
    }
    // See `PopoverFocusState`: refuse to reopen something that was just
    // closed by the same click racing this handler.
    if app.state::<PopoverFocusState>().recently_hidden_externally() {
        return;
    }

    let scale = window.scale_factor().unwrap_or(1.0);
    let icon_pos = icon_rect.position.to_physical::<f64>(scale);
    let icon_size = icon_rect.size.to_physical::<f64>(scale);
    if let Ok(popover_size) = window.outer_size() {
        let mut x = icon_pos.x + (icon_size.width / 2.0) - (popover_size.width as f64 / 2.0);
        let mut y = icon_pos.y + icon_size.height + 4.0;

        // Clamp to the work area of whichever monitor the tray icon is on
        // (falling back to the primary monitor if that lookup fails) —
        // uncorrected, an icon near the right end of the menu bar, or on a
        // second display, puts part or all of the popover off screen.
        // 8px margin from every edge; `.max(min_*)` on the upper bound
        // guards a popover wider/taller than the work area itself, which
        // would otherwise invert the clamp and push it off the opposite
        // edge.
        // `monitor_from_point` takes LOGICAL points: tao's macOS impl tests
        // the point against `CGDisplayBounds`, which is in points, while
        // `icon_pos` here is physical. Passing physical pixels made the
        // lookup miss every display on a Retina Mac once the icon was past
        // half the screen width, silently falling back to the primary
        // monitor — which is exactly the multi-display case this clamp
        // exists for. The arithmetic below stays physical, like
        // `work_area()`, `position()` and `outer_size()`.
        let icon_pos_logical = icon_rect.position.to_logical::<f64>(scale);
        let monitor = window
            .monitor_from_point(icon_pos_logical.x, icon_pos_logical.y)
            .ok()
            .flatten()
            .or_else(|| window.primary_monitor().ok().flatten());
        if let Some(monitor) = monitor {
            let work_area = monitor.work_area();
            let margin = 8.0;
            let min_x = work_area.position.x as f64 + margin;
            let min_y = work_area.position.y as f64 + margin;
            let max_x = (work_area.position.x as f64 + work_area.size.width as f64 - popover_size.width as f64 - margin).max(min_x);
            let max_y = (work_area.position.y as f64 + work_area.size.height as f64 - popover_size.height as f64 - margin).max(min_y);
            x = x.clamp(min_x, max_x);
            y = y.clamp(min_y, max_y);
        }

        let _ = window.set_position(Position::Physical(PhysicalPosition { x: x.round() as i32, y: y.round() as i32 }));
    }
    let _ = window.show();
    let _ = window.set_focus();
    install_click_outside_monitor(app);
}

thread_local! {
    /// The global mouse-down monitor installed by
    /// `install_click_outside_monitor` while the popover is visible, or
    /// `None` when it isn't. A thread-local rather than app-managed state
    /// (`app.manage()`, like `PopoverFocusState` above): the underlying
    /// `NSEvent` monitor object is an Objective-C object with no Send/Sync
    /// guarantee, which `app.manage()` requires. Every touch point
    /// (install, remove, and the monitor's own handler firing) already runs
    /// on the main thread — AppKit requires that for all of them — so a
    /// thread-local needs no extra synchronization and is exactly as safe.
    static CLICK_OUTSIDE_MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Installs a global mouse-down monitor that hides the tray popover the
/// moment a click happens *outside this app entirely* — another
/// application's window, or the desktop. This is the fix for this packet's
/// actual bug (see `toggle_popover`'s doc comment for the evidence that
/// `set_focus()` alone can't be trusted): `NSEvent
/// addGlobalMonitorForEventsMatchingMask:handler:` doesn't depend on the
/// popover ever becoming the key window, because Apple's docs guarantee it
/// fires only for events landing outside our own app, key window or not.
///
/// That scope is also its limit: per the same docs, it does *not* fire for
/// a click landing on one of our own other windows (the main window) —
/// that's still "inside this app" from AppKit's point of view. That case
/// stays covered by lib.rs's `WindowEvent::Focused(false)` handler, which
/// is a same-app key-window handoff and, unlike cross-app activation, is
/// simple enough for macOS to report reliably.
///
/// Any previously-installed monitor is removed first, so calling this
/// on every open (rather than trying to track whether one is already
/// running) never leaks a second one alongside it.
fn install_click_outside_monitor(app: &AppHandle) {
    remove_click_outside_monitor();
    let app_for_block = app.clone();
    let block = RcBlock::new(move |_event: NonNull<NSEvent>| {
        // Apple's docs guarantee this handler runs on the main thread, so
        // touching the window and app state directly here — rather than
        // hopping through `run_on_main_thread`, as code elsewhere in this
        // file does for calls that might originate off it — is safe.
        if let Some(window) = app_for_block.get_webview_window(POPOVER_LABEL) {
            if matches!(window.is_visible(), Ok(true)) {
                let _ = window.hide();
                // A click on the tray icon itself is *also* an "outside"
                // click as far as this monitor is concerned (see
                // `PopoverFocusState`'s doc comment for the full race):
                // it fires on that click's mouse-DOWN half, slightly
                // ahead of the tray icon's own click handler seeing the
                // mouse-UP half. Stamping the guard here — not only from
                // `Focused(false)` — is what stops that handler from
                // mistaking "already hidden by this monitor" for "the
                // user wants it reopened".
                app_for_block.state::<PopoverFocusState>().note_external_hide();
            }
        }
        remove_click_outside_monitor();
    });
    let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block);
    CLICK_OUTSIDE_MONITOR.with(|cell| *cell.borrow_mut() = monitor);
}

/// Removes the monitor installed by `install_click_outside_monitor`, if one
/// is currently installed — a no-op otherwise. Called from every path that
/// hides the popover (the tray icon's own re-click, lib.rs's
/// `Focused(false)` handler, and the monitor's own handler right after it
/// fires once) so a stray monitor never keeps running once the popover is
/// closed. The one gap: TrayPopover.svelte's Escape handler hides the
/// window directly from JS with no Rust-side hook to call this from —
/// outside this packet's file boundaries to add one. Left dangling, the
/// monitor just fires (harmlessly — the window is already hidden, so its
/// own `is_visible()` check no-ops the rest) on the next unrelated outside
/// click anywhere and removes itself there instead; nothing leaks past
/// that one click.
pub fn remove_click_outside_monitor() {
    CLICK_OUTSIDE_MONITOR.with(|cell| {
        if let Some(monitor) = cell.borrow_mut().take() {
            // SAFETY: `monitor` is exactly the object
            // `addGlobalMonitorForEventsMatchingMask_handler` returned to
            // us, taken once here and not reused afterward.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    });
}

fn show_window(app: &AppHandle) {
    // Unhide the APPLICATION first — Tauri's own macOS call. A menu-bar app
    // is hidden as a process, so showing one of its windows while the app
    // itself is hidden leaves the window behind everything.
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        // Last resort, and the one that always works: briefly pinning the
        // window above others raises it even when activation is refused.
        let _ = window.set_always_on_top(true);
        let _ = window.set_always_on_top(false);
        // Showing and focusing a window is not the same as bringing the
        // APPLICATION forward: a background app stays behind whatever the
        // user is looking at, so "Open" and "Check for updates…" appeared to
        // do nothing. Activating it is the missing half.
        activate_app();
    }
}

/// `[NSApp activateIgnoringOtherApps: YES]` — the app is launched from the
/// menu bar, so nothing else brings it to the front.
fn activate_app() {
    use objc2::runtime::AnyClass;
    use objc2::{msg_send, sel};
    // Sent by name rather than through objc2-app-kit's NSApplication, whose
    // feature set here is trimmed to the event APIs the click-outside
    // monitor needs.
    unsafe {
        if let Some(cls) = AnyClass::get(c"NSApplication") {
            let ns_app: *mut AnyObject = msg_send![cls, sharedApplication];
            if ns_app.is_null() {
                return;
            }
            // macOS 14 deprecated `activateIgnoringOtherApps:` and largely
            // ignores it for an accessory/menu-bar app, which is why the
            // window kept appearing behind everything. `activate` is the
            // replacement; fall back only where it does not exist.
            let responds: bool = msg_send![ns_app, respondsToSelector: sel!(activate)];
            if responds {
                let _: () = msg_send![ns_app, activate];
            } else {
                #[allow(deprecated)]
                let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
            }
        }
    }
}

/// Sets the tray title to the unseen count (macOS shows this as text next
/// to the icon), or clears it back to just the icon when there's nothing
/// unseen.
///
/// Fire-and-forget, and off the calling thread on purpose: `unseen_count`
/// takes the engine's store mutex, which a poll in flight is holding, so
/// counting inline would freeze whoever asked — including the main thread,
/// which is where the tray's own menu handler runs.
pub fn refresh_tray_badge(app: &AppHandle, engine: &Arc<dyn EngineApi>) {
    let app = app.clone();
    let engine = engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let count = engine.unseen_count();
        let title = if count == 0 {
            None
        } else if count > 99 {
            Some("99+".to_string())
        } else {
            Some(count.to_string())
        };
        // Tray mutation belongs on the main thread on macOS.
        let inner = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(tray) = inner.tray_by_id(TRAY_ID) {
                let _ = tray.set_title(title);
            }
        });
    });
}

/// REVERTED 2026-09-07 (same day it went in — see Cargo.toml's comment on
/// the `tauri` dependency for the full chain): the popover window was
/// briefly `"transparent": true` in tauri.conf.json behind the
/// `macos-private-api` Cargo feature and `app.macOSPrivateApi: true`, so
/// TrayPopover.svelte's `.popover` rounded corners and a real drop shadow
/// could show through. Both flags are gone now — a private API means the
/// app can't be accepted to the Mac App Store, and the maintainer wants
/// that door left open. The popover is back to an opaque, square panel
/// (`"transparent": false`); don't re-add either flag without re-checking
/// that tradeoff first.
///
/// `tauri.conf.json`'s per-window `"shadow": true` for `tray-popover` is
/// left as-is: it asks tao/wry to set `NSWindow.hasShadow = true`, and on
/// an opaque square window that's just a plain square glow behind the
/// panel, independent of the (now removed) rounded-card CSS.
///
/// The popover-route CSS (app.css's `[data-route="tray"]` rule, set by
/// main.ts) still exists — it now paints the single light-sage surface
/// instead of working around a transparent window.
pub fn setup(app: &App) -> tauri::Result<()> {
    app.manage(PopoverFocusState::new());

    let poll_item = MenuItem::with_id(app, "poll_now", "Poll now", true, None::<&str>)?;
    let open_item = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    // Updates are reachable from the menu bar too, not only from a banner in
    // a window someone may never open. The menu never installs anything: it
    // shows the window and asks it to check, so the decision stays in one
    // place, with the version named and consent asked for there.
    let updates_item =
        MenuItem::with_id(app, "check_updates", "Check for updates…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu =
        Menu::with_items(app, &[&poll_item, &open_item, &updates_item, &quit_item])?;

    let icon = tauri::image::Image::from_bytes(TRAY_ICON_BYTES)?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(false)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Vigie")
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } =
                event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        })
        .build(app)?;

    let engine = app.state::<EngineState>().0.clone();
    refresh_tray_badge(app.handle(), &engine);

    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id.as_ref() {
        // This handler runs on the main thread, so the poll itself cannot:
        // it is a network round trip per repo. Spawned, and its result is
        // announced exactly as a scheduled poll's is.
        "poll_now" => {
            let engine = app.state::<EngineState>().0.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = poller::poll_and_notify(app.clone(), engine.clone()).await {
                    eprintln!("vigie: tray poll failed: {e}");
                }
                refresh_tray_badge(&app, &engine);
            });
        }
        "open" => show_window(app),
        "check_updates" => {
            show_window(app);
            let _ = app.emit("check-for-updates", ());
        }
        "quit" => app.exit(0),
        _ => {}
    }
}
