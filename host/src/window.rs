//! macOS render window. Opens a native window sized from the first frame and
//! blits the latest frame each tick. The window owns the main thread; the network
//! thread decodes each frame (JPEG or H.264) and drops the latest one into a
//! shared slot, so a brief backlog collapses to the current frame instead of
//! replaying stale ones. It also grabs mouse/keyboard input and forwards it as
//! device-space messages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use minifb::{MouseButton, MouseMode, ScaleMode, Window, WindowOptions};

use crate::clipboard;
use crate::input::{map_to_norm, InputFrame};
use crate::protocol::{self, KeyCode, MessageType, SystemAction, TouchPhase};
use crate::sidebar;
use crate::video::DecodedFrame;

/// Where the network thread drops the most recent decoded frame (latest only).
pub type FrameSlot = Arc<Mutex<Option<DecodedFrame>>>;

pub fn new_frame_slot() -> FrameSlot {
    Arc::new(Mutex::new(None))
}

fn send_action(tx: &Sender<InputFrame>, action: SystemAction) {
    let _ = tx.send(InputFrame::new(
        MessageType::SystemAction,
        protocol::encode_system_action(action),
    ));
}

fn send_key(tx: &Sender<InputFrame>, code: protocol::KeyCode) {
    let _ = tx.send(InputFrame::new(
        MessageType::InputKey,
        protocol::encode_key(code),
    ));
}

/// Per-session clipboard bookkeeping, shared between the change poll and the Cmd+V
/// handler. A change whose hash we already synced is just the echo of what the
/// device gave us (or our own paste), so we don't re-send it.
#[derive(Default)]
struct ClipState {
    last_change_count: i64,
    last_synced_hash: Option<u64>,
}

/// Send a clipboard set: `[flags:u8][utf8]`, flags bit0 means paste after setting.
fn send_clipboard_set(tx: &Sender<InputFrame>, text: &str, paste: bool) {
    let mut p = Vec::with_capacity(1 + text.len());
    p.push(if paste { 0x01 } else { 0x00 });
    p.extend_from_slice(text.as_bytes());
    let _ = tx.send(InputFrame::new(MessageType::ClipboardSet, p));
}

/// Push the host clipboard to the device then paste it, so cross-device paste
/// carries the right text. Falls back to a plain iOS paste if the host
/// clipboard isn't usable text (empty, too large, or no display). Shared by
/// the Cmd/Ctrl+V shortcut and the sidebar Paste button.
fn paste_now(tx: &Sender<InputFrame>, clip: &Arc<Mutex<ClipState>>) {
    match clipboard::read_text() {
        Some(text) if !text.is_empty() && text.len() <= clipboard::MAX_CLIPBOARD_BYTES => {
            if let Ok(mut st) = clip.lock() {
                st.last_synced_hash = Some(clipboard::hash_text(&text));
            }
            send_clipboard_set(tx, &text, true);
        }
        _ => send_key(tx, KeyCode::Paste),
    }
}

/// Translate a sidebar button press into the same messages its matching
/// keyboard shortcut sends.
fn dispatch_sidebar_action(tx: &Sender<InputFrame>, clip: &Arc<Mutex<ClipState>>, action: sidebar::Action) {
    use sidebar::Action::*;
    match action {
        Home => send_action(tx, SystemAction::Home),
        Lock => send_action(tx, SystemAction::Lock),
        AppSwitcher => send_action(tx, SystemAction::AppSwitcher),
        Rotate => send_action(tx, SystemAction::RotateLeft),
        Back => send_action(tx, SystemAction::Back),
        SelectAll => send_key(tx, KeyCode::SelectAll),
        Copy => send_key(tx, KeyCode::Copy),
        Paste => paste_now(tx, clip),
        Cut => send_key(tx, KeyCode::Cut),
        Undo => send_key(tx, KeyCode::Undo),
    }
}

/// Put a clipboard value the device sent into the Mac pasteboard, recording the
/// hash and change count so our own poll doesn't echo it back.
fn apply_remote_clipboard(text: &str, clip: &Arc<Mutex<ClipState>>) {
    if text.is_empty() {
        return;
    }
    let h = clipboard::hash_text(text);
    let mut st = clip.lock().unwrap();
    if st.last_synced_hash == Some(h) {
        return; // already have it
    }
    st.last_synced_hash = Some(h);
    st.last_change_count = clipboard::write_text(text);
}

/// If the Mac clipboard changed to new text, push it to the device.
fn poll_clipboard(tx: &Sender<InputFrame>, clip: &Arc<Mutex<ClipState>>) {
    let cc = clipboard::change_count();
    {
        let mut st = clip.lock().unwrap();
        if cc == st.last_change_count {
            return;
        }
        st.last_change_count = cc;
    }
    let text = match clipboard::read_text() {
        Some(t) if !t.is_empty() && t.len() <= clipboard::MAX_CLIPBOARD_BYTES => t,
        _ => return,
    };
    let h = clipboard::hash_text(&text);
    {
        let mut st = clip.lock().unwrap();
        if st.last_synced_hash == Some(h) {
            return; // already in sync (our own paste / device echo)
        }
        st.last_synced_hash = Some(h);
    }
    send_clipboard_set(tx, &text, false);
}

#[cfg(target_os = "macos")]
unsafe fn nsstring_to_string(ns: *mut objc::runtime::Object) -> String {
    use objc::{msg_send, sel, sel_impl};
    let utf8: *const std::os::raw::c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_string_lossy()
        .into_owned()
}

/// Capture the Mac keyboard and forward it to the device. minifb only implements
/// `keyDown:`, and macOS routes Cmd+letter through `performKeyEquivalent:` which
/// minifb never sees, so a local event monitor is the only way to reliably see
/// every key-down. We sort each one out: Cmd+J/L/T/R are system actions,
/// Cmd+A/C/V/X/Z are iOS editing shortcuts, Enter/Backspace/Tab/Esc/arrows are
/// editing keys, everything else is text (the layout-resolved characters, so any
/// keyboard layout already gave us the right glyph). Handled events are swallowed.
#[cfg(target_os = "macos")]
fn install_key_monitor(tx: Sender<InputFrame>, clip: Arc<Mutex<ClipState>>) {
    use block::ConcreteBlock;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    const NS_EVENT_MASK_KEY_DOWN: u64 = 1 << 10;
    const NS_CMD: u64 = 1 << 20; // NSEventModifierFlagCommand

    let handler = ConcreteBlock::new(move |event: *mut Object| -> *mut Object {
        let nil: *mut Object = std::ptr::null_mut();
        unsafe {
            let flags: u64 = msg_send![event, modifierFlags];
            let repeat: bool = msg_send![event, isARepeat];

            if (flags & NS_CMD) != 0 {
                if repeat {
                    return nil; // Cmd combos never auto-repeat into the device
                }
                let chars: *mut Object = msg_send![event, charactersIgnoringModifiers];
                let len: u64 = if chars.is_null() {
                    0
                } else {
                    msg_send![chars, length]
                };
                if len >= 1 {
                    let ch: u16 = msg_send![chars, characterAtIndex: 0u64];
                    let mut handled = true;
                    match ch as u8 {
                        b'j' | b'J' => send_action(&tx, SystemAction::Home),
                        b'l' | b'L' => send_action(&tx, SystemAction::Lock),
                        b't' | b'T' => send_action(&tx, SystemAction::AppSwitcher),
                        b'r' | b'R' => send_action(&tx, SystemAction::RotateLeft),
                        b'a' | b'A' => send_key(&tx, KeyCode::SelectAll),
                        b'c' | b'C' => send_key(&tx, KeyCode::Copy),
                        b'v' | b'V' => paste_now(&tx, &clip),
                        b'x' | b'X' => send_key(&tx, KeyCode::Cut),
                        b'z' | b'Z' => send_key(&tx, KeyCode::Undo),
                        _ => handled = false,
                    }
                    if handled {
                        return nil;
                    }
                }
                return event; // other Cmd combos: let macOS have them
            }

            // Editing keys (no Cmd), by macOS virtual keycode.
            let keycode: u16 = msg_send![event, keyCode];
            // Esc maps to the iOS back gesture, not a literal Escape key.
            if keycode == 53 {
                send_action(&tx, SystemAction::Back);
                return nil;
            }
            let special = match keycode {
                36 | 76 => Some(KeyCode::Enter),
                51 => Some(KeyCode::Backspace),
                48 => Some(KeyCode::Tab),
                123 => Some(KeyCode::Left),
                124 => Some(KeyCode::Right),
                125 => Some(KeyCode::Down),
                126 => Some(KeyCode::Up),
                _ => None,
            };
            if let Some(k) = special {
                send_key(&tx, k);
                return nil;
            }

            // Everything else is text: forward the resolved characters.
            let chars: *mut Object = msg_send![event, characters];
            if !chars.is_null() {
                let s = nsstring_to_string(chars);
                if !s.is_empty() && !s.chars().all(char::is_control) {
                    let _ = tx.send(InputFrame::new(
                        MessageType::InputText,
                        protocol::encode_text(&s),
                    ));
                    return nil;
                }
            }
        }
        event
    });
    let handler = handler.copy();
    unsafe {
        let _: *mut Object = msg_send![class!(NSEvent),
            addLocalMonitorForEventsMatchingMask: NS_EVENT_MASK_KEY_DOWN
            handler: &*handler];
    }
    std::mem::forget(handler);
}

#[cfg(not(target_os = "macos"))]
fn install_key_monitor(_tx: Sender<InputFrame>, _clip: Arc<Mutex<ClipState>>) {}

/// Forwards typed characters from minifb's input callback to the device as text.
/// It tracks Ctrl from the same key-event stream (via `set_key_state`) and drops
/// characters while Ctrl is held, so a shortcut like Ctrl+C isn't also typed. This
/// is needed because minifb's backends disagree: X11 hands us a C0 control code for
/// Ctrl+letter (caught by `is_control`), but Wayland resolves the bare keysym and
/// hands us the plain letter, which `is_control` wouldn't catch.
#[cfg(not(target_os = "macos"))]
struct TextForwarder {
    tx: Sender<InputFrame>,
    ctrl: bool,
}

#[cfg(not(target_os = "macos"))]
impl minifb::InputCallback for TextForwarder {
    fn add_char(&mut self, uni_char: u32) {
        if self.ctrl {
            return;
        }
        if let Some(c) = char::from_u32(uni_char) {
            if !c.is_control() {
                let _ = self
                    .tx
                    .send(InputFrame::new(MessageType::InputText, protocol::encode_text(&c.to_string())));
            }
        }
    }

    fn set_key_state(&mut self, key: minifb::Key, state: bool) {
        if matches!(key, minifb::Key::LeftCtrl | minifb::Key::RightCtrl) {
            self.ctrl = state;
        }
    }
}

/// Register text-input forwarding on the window. minifb's callback is per-window,
/// so this must run again after the window is recreated on rotation. macOS uses
/// the global `install_key_monitor` instead, so there it's a no-op.
#[cfg(not(target_os = "macos"))]
fn attach_text_input(window: &mut Window, tx: &Sender<InputFrame>) {
    window.set_input_callback(Box::new(TextForwarder {
        tx: tx.clone(),
        ctrl: false,
    }));
}

#[cfg(target_os = "macos")]
fn attach_text_input(_window: &mut Window, _tx: &Sender<InputFrame>) {}

/// Poll this frame's keys and forward shortcuts and editing keys, mirroring the
/// macOS monitor but with Ctrl as the modifier (macOS uses Cmd). Text itself goes
/// through `TextForwarder`; here we handle Esc (back), Enter/Tab, the repeating
/// Backspace/arrows, and the Ctrl combos. macOS routes all of this through
/// `install_key_monitor`, so this is Linux/Windows only.
#[cfg(not(target_os = "macos"))]
fn pump_keys(window: &Window, tx: &Sender<InputFrame>, clip: &Arc<Mutex<ClipState>>) {
    use minifb::{Key, KeyRepeat};

    let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);

    // One-shot keys: shortcuts and the editing keys that shouldn't auto-repeat.
    for key in window.get_keys_pressed(KeyRepeat::No) {
        if ctrl {
            match key {
                Key::J => send_action(tx, SystemAction::Home),
                Key::L => send_action(tx, SystemAction::Lock),
                Key::T => send_action(tx, SystemAction::AppSwitcher),
                Key::R => send_action(tx, SystemAction::RotateLeft),
                Key::A => send_key(tx, KeyCode::SelectAll),
                Key::C => send_key(tx, KeyCode::Copy),
                Key::V => paste_now(tx, clip),
                Key::X => send_key(tx, KeyCode::Cut),
                Key::Z => send_key(tx, KeyCode::Undo),
                _ => {}
            }
        } else {
            match key {
                Key::Escape => send_action(tx, SystemAction::Back),
                Key::Enter | Key::NumPadEnter => send_key(tx, KeyCode::Enter),
                Key::Tab => send_key(tx, KeyCode::Tab),
                _ => {}
            }
        }
    }

    // Repeating keys: held Backspace and arrows keep firing. On a key's first frame
    // it shows up in both KeyRepeat::No and ::Yes, so these keys must stay out of the
    // one-shot match above, or they'd fire twice that frame. Keep the two sets disjoint.
    if !ctrl {
        for key in window.get_keys_pressed(KeyRepeat::Yes) {
            match key {
                Key::Backspace => send_key(tx, KeyCode::Backspace),
                Key::Left => send_key(tx, KeyCode::Left),
                Key::Right => send_key(tx, KeyCode::Right),
                Key::Up => send_key(tx, KeyCode::Up),
                Key::Down => send_key(tx, KeyCode::Down),
                _ => {}
            }
        }
    }
}

/// Window size for a `w`x`h` device frame at the given scale, clamped to at most
/// about 92% of the screen while keeping the aspect ratio. `sidebar_w` is a
/// fixed-width budget (the button panel) added on top of the scaled content,
/// not stretched with it; the returned width includes it.
fn scaled_fit(w: usize, h: usize, scale: f32, sidebar_w: usize) -> (usize, usize) {
    let (sw, sh) = screen_visible_size();
    let (max_w, max_h) = (
        ((sw * 0.92) as f32 - sidebar_w as f32).max(160.0),
        (sh * 0.92) as f32,
    );
    let tw = w as f32 * scale;
    let th = h as f32 * scale;
    let clamp = (max_w / tw).min(max_h / th).min(1.0);
    (
        ((tw * clamp) as usize).max(160) + sidebar_w,
        ((th * clamp) as usize).max(160),
    )
}

/// Open a render window with the given content size. We recreate the window on
/// rotation instead of resizing it, because minifb only updates its tracked size
/// on a live user drag. A programmatic resize would leave the size (and so the
/// rendering and touch mapping) stale.
fn open_window(title: &str, w: usize, h: usize) -> Result<Window> {
    let mut window = Window::new(
        title,
        w,
        h,
        WindowOptions {
            resize: true,
            // The buffer we present is already window-sized and letterboxed (see
            // `scale_frame`), so Stretch blits it 1:1.
            scale_mode: ScaleMode::Stretch,
            ..WindowOptions::default()
        },
    )
    .context("could not open the render window")?;
    window.set_target_fps(60);
    Ok(window)
}

/// minifb (on Windows especially) can hand back a client area whose aspect ratio
/// differs from what we asked for, which would letterbox the device with black
/// bars even at the default size. Snap the window down to the largest
/// device-aspect rectangle that fits inside whatever size minifb actually gave,
/// re-opening until the aspect matches (or we give up after a few tries). This
/// makes the default window match the device's shape — scrcpy-style, so there are
/// no bars unless the user later resizes to a different aspect.
fn fit_window_to_aspect(mut window: Window, title: &str, frame_w: usize, frame_h: usize) -> Result<Window> {
    if frame_w == 0 || frame_h == 0 {
        return Ok(window);
    }
    let target = frame_w as f32 / frame_h as f32;
    for _ in 0..3 {
        let (gw, gh) = window.get_size();
        if gw == 0 || gh == 0 {
            break;
        }
        let cur = gw as f32 / gh as f32;
        if (cur - target).abs() <= target * 0.01 {
            break; // already device aspect within 1%
        }
        // Largest device-aspect rectangle that fits inside the current client box.
        let (nw, nh) = if cur > target {
            (((gh as f32 * target).round() as usize).max(160), gh) // too wide -> pin height
        } else {
            (gw, ((gw as f32 / target).round() as usize).max(160)) // too tall -> pin width
        };
        if (nw, nh) == (gw, gh) {
            break;
        }
        let pos = window.get_position();
        let mut w = open_window(title, nw, nh)?;
        w.set_position(pos.0, pos.1);
        window = w;
    }
    Ok(window)
}

/// Become a regular Dock app and set its icon (the icon is embedded in the
/// binary). Call this before the window opens so the Dock shows our icon from the
/// start instead of briefly flashing the default executable one. Main thread only.
#[cfg(target_os = "macos")]
pub fn set_app_icon() {
    use objc::runtime::{Object, BOOL};
    use objc::{class, msg_send, sel, sel_impl};
    const ICON: &[u8] = include_bytes!("../assets/AppIcon.png");
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        // NSApplicationActivationPolicyRegular = 0.
        let _: BOOL = msg_send![app, setActivationPolicy: 0isize];
        let data: *mut Object =
            msg_send![class!(NSData), dataWithBytes: ICON.as_ptr() length: ICON.len()];
        if data.is_null() {
            return;
        }
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let _: () = msg_send![app, setApplicationIconImage: image];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_icon() {}

/// Make the process DPI-aware on Windows so the window and mirror render at true
/// pixels instead of being bitmap-scaled up by the display's scale factor (which
/// makes everything look zoomed and blurry). No-op elsewhere. Must run before any
/// window is created.
#[cfg(windows)]
fn enable_dpi_awareness() {
    #[link(name = "user32")]
    extern "system" {
        fn SetProcessDPIAware() -> i32;
    }
    unsafe {
        SetProcessDPIAware();
    }
}
#[cfg(not(windows))]
fn enable_dpi_awareness() {}

/// The window's backing scale (2.0 on retina). minifb reports its size in points
/// but samples with nearest-neighbor, so to stay sharp we scale our buffer up to
/// the real backing pixel resolution.
#[cfg(target_os = "macos")]
fn backing_scale(window: &Window) -> f32 {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let handle = window.get_window_handle();
        if handle.is_null() {
            return 2.0;
        }
        let nswindow = handle as *mut Object;
        let scale: f64 = msg_send![nswindow, backingScaleFactor];
        if scale >= 1.0 {
            scale as f32
        } else {
            2.0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn backing_scale(_window: &Window) -> f32 {
    1.0
}

/// The main screen's usable size in points (menu bar and dock excluded). Used to
/// fit the window on screen. Otherwise macOS clamps an over-tall window's height
/// and the device ends up letterboxed with side bars.
#[cfg(target_os = "macos")]
fn screen_visible_size() -> (f64, f64) {
    use cocoa::foundation::NSRect;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            return (1440.0, 900.0);
        }
        let frame: NSRect = msg_send![screen, visibleFrame];
        (frame.size.width, frame.size.height)
    }
}

#[cfg(windows)]
fn screen_visible_size() -> (f64, f64) {
    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(n_index: i32) -> i32;
    }
    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        if w > 0 && h > 0 {
            (w as f64, h as f64)
        } else {
            (1440.0, 900.0)
        }
    }
}
#[cfg(not(any(target_os = "macos", windows)))]
fn screen_visible_size() -> (f64, f64) {
    (1440.0, 900.0)
}

/// Bilinearly scale `src` into the `[0, content_w)` columns of `dst`, a
/// `stride`-wide by `out_h`-tall buffer that also holds the sidebar past
/// `content_w`. Aspect-fit (letterbox) like scrcpy: the whole device screen is
/// always visible, centered, with black bars on the off-aspect axis; `dst` must
/// already be cleared to black. The window is snapped to the device aspect at
/// open (`fit_window_to_aspect`), so the default has no bars — they appear only
/// if you resize to a different shape, exactly like scrcpy. `map_to_norm` maps
/// clicks against this same content rect so taps still land where the cursor is.
fn scale_frame(src: &DecodedFrame, content_w: usize, out_h: usize, stride: usize, dst: &mut [u32]) {
    let content_w = content_w.max(1);
    let out_h = out_h.max(1);

    let scale = (content_w as f32 / src.width as f32).min(out_h as f32 / src.height as f32);
    let dw = ((src.width as f32 * scale) as usize).clamp(1, content_w);
    let dh = ((src.height as f32 * scale) as usize).clamp(1, out_h);
    let x_off = (content_w - dw) / 2;
    let y_off = (out_h - dh) / 2;

    let inv_x = src.width as f32 / dw as f32;
    let inv_y = src.height as f32 / dh as f32;
    let last_x = src.width - 1;
    let last_y = src.height - 1;
    for y in 0..dh {
        let fy = ((y as f32 + 0.5) * inv_y - 0.5).max(0.0);
        let y0 = (fy as usize).min(last_y);
        let y1 = (y0 + 1).min(last_y);
        let wy = fy - y0 as f32;
        let srow0 = y0 * src.width;
        let srow1 = y1 * src.width;
        let drow = (y + y_off) * stride + x_off;
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * inv_x - 0.5).max(0.0);
            let x0 = (fx as usize).min(last_x);
            let x1 = (x0 + 1).min(last_x);
            let wx = fx - x0 as f32;
            dst[drow + x] = bilerp(
                src.buf[srow0 + x0],
                src.buf[srow0 + x1],
                src.buf[srow1 + x0],
                src.buf[srow1 + x1],
                wx,
                wy,
            );
        }
    }
}

/// Bilinear blend of four `0x00RRGGBB` pixels.
#[inline]
fn bilerp(p00: u32, p10: u32, p01: u32, p11: u32, wx: f32, wy: f32) -> u32 {
    let mut out = 0u32;
    let mut shift = 0;
    while shift <= 16 {
        let c00 = ((p00 >> shift) & 0xff) as f32;
        let c10 = ((p10 >> shift) & 0xff) as f32;
        let c01 = ((p01 >> shift) & 0xff) as f32;
        let c11 = ((p11 >> shift) & 0xff) as f32;
        let top = c00 + (c10 - c00) * wx;
        let bot = c01 + (c11 - c01) * wx;
        let v = (top + (bot - top) * wy) as u32;
        out |= (v & 0xff) << shift;
        shift += 8;
    }
    out
}

/// Run the window loop on the calling (main) thread until the window closes or
/// `stop` is set. Blits the latest frame and forwards input each iteration.
pub fn run_window(
    title: &str,
    frames: FrameSlot,
    stop: Arc<AtomicBool>,
    input_tx: Sender<InputFrame>,
    clip_in: Receiver<String>,
) -> Result<()> {
    enable_dpi_awareness();
    let first = match wait_for_first_frame(&frames, &stop) {
        Some(f) => f,
        None => return Ok(()), // stopped before the first frame arrived
    };

    // Compute the scale from the device's intrinsic size (long and short sides)
    // so it's the same whatever orientation we launch in. A landscape-first launch
    // must size the same as portrait-then-landscape.
    let (sw, sh) = screen_visible_size();
    let (sw, sh) = (sw as f32, sh as f32);
    let long = first.width.max(first.height).max(1) as f32;
    let short = first.width.min(first.height).max(1) as f32;
    // Cap at 1.0 so the window never opens larger than the device's native
    // resolution — upscaling past native only softens the image (the phone has
    // no more pixels), which reads as "pixelated" at big sizes.
    let display_scale = ((sh * 0.80) / long).min((sw * 0.80) / short).min(1.0);
    // Size the first window exactly as a rotation to this shape would.
    let (win_w, win_h) = scaled_fit(first.width, first.height, display_scale, sidebar::WIDTH);
    let mut window = open_window(title, win_w, win_h)?;
    // Snap to the device aspect so the default has no black bars even if minifb
    // handed back an off-aspect client area (scrcpy-style default fit).
    window = fit_window_to_aspect(window, title, first.width, first.height)?;
    let clip = Arc::new(Mutex::new(ClipState::default()));
    install_key_monitor(input_tx.clone(), clip.clone());
    attach_text_input(&mut window, &input_tx);

    let mut current = first;
    let mut last_dims = (current.width, current.height);
    let mut input = InputState::default();
    let mut combined: Vec<u32> = Vec::new();
    let mut last_clip_poll = Instant::now();
    while window.is_open() && !stop.load(Ordering::Relaxed) {
        // Take the newest decoded frame, dropping any older one.
        if let Some(decoded) = frames.lock().unwrap().take() {
            current = decoded;
        }

        // If the frame's shape flipped the phone rotated. Reopen the window at the
        // new size, keeping its place. minifb won't track a programmatic resize, so
        // recreating it is the reliable way to follow the rotation.
        if (current.width, current.height) != last_dims {
            last_dims = (current.width, current.height);
            let (nw, nh) = scaled_fit(current.width, current.height, display_scale, sidebar::WIDTH);
            let pos = window.get_position();
            if let Ok(mut w) = open_window(title, nw, nh) {
                w.set_position(pos.0, pos.1);
                attach_text_input(&mut w, &input_tx);
                window = w;
            }
        }

        // Window points: the sidebar is a fixed-width strip on the right, the
        // device frame and touch mapping get whatever's left.
        let (gw, gh) = window.get_size();
        let content_w = gw.saturating_sub(sidebar::WIDTH).max(1);

        let input_ctx = InputCtx { content_w, win_h: gh, tx: &input_tx, clip: &clip };
        pump_input(&mut window, &current, &mut input, &input_ctx);
        #[cfg(not(target_os = "macos"))]
        pump_keys(&window, &input_tx, &clip);

        // Render a window-sized, letterboxed buffer at the backing (retina)
        // resolution for minifb to blit 1:1. Both axes are capped together so a
        // huge window keeps its aspect (and per-frame cost bounded) under Stretch.
        // Content and sidebar are written directly into one shared buffer, side
        // by side, instead of composing separate buffers and copying them
        // together every frame.
        let bs = backing_scale(&window);
        let ow = (content_w as f32 * bs).max(1.0);
        let oh = (gh as f32 * bs).max(1.0);
        let down = (2600.0 / (ow + sidebar::WIDTH as f32 * bs)).min(2600.0 / oh).min(1.0);
        let content_px = ((ow * down) as usize).max(1);
        let total_h = ((oh * down) as usize).max(1);
        let sb_px = (sidebar::WIDTH as f32 * bs * down).round() as usize;
        let stride = content_px + sb_px;

        combined.clear();
        combined.resize(stride * total_h, 0);
        scale_frame(&current, content_px, total_h, stride, &mut combined);
        sidebar::draw_into(&mut combined, stride, content_px, sb_px, total_h, input.sidebar_down);

        window
            .update_with_buffer(&combined, stride, total_h)
            .context("failed to present a frame")?;

        // Push Mac clipboard changes to the device (rate-limited).
        if last_clip_poll.elapsed() >= Duration::from_millis(300) {
            last_clip_poll = Instant::now();
            poll_clipboard(&input_tx, &clip);
        }
        // Apply iPhone to Mac clipboard changes here on the main thread.
        while let Ok(text) = clip_in.try_recv() {
            apply_remote_clipboard(&text, &clip);
        }
    }

    stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// A synthetic left-edge "swipe to go back" gesture, emitted as timed touch steps
/// across frames so the device's interactive-pop recognizer sees real motion.
/// Fired alongside the Cmd+[ Back action so apps that ignore the key command still
/// navigate back. Entirely host-side (reuses the normal touch path) — no device
/// tweak change needed. Apps that honor both Cmd+[ and the swipe go back twice.
struct BackSwipe {
    start: Instant,
    /// (delay-from-start, phase, x, y), in emit order: Down at the left edge, a
    /// run of Moves rightward past the halfway point, then Up.
    steps: Vec<(Duration, TouchPhase, f32, f32)>,
    next: usize,
}

impl BackSwipe {
    fn new() -> Self {
        let (y, x0, x1, n) = (0.5f32, 0.02f32, 0.62f32, 10u64);
        let mut steps = Vec::with_capacity(n as usize + 2);
        steps.push((Duration::from_millis(0), TouchPhase::Down, x0, y));
        for i in 1..=n {
            let x = x0 + (x1 - x0) * (i as f32 / n as f32);
            steps.push((Duration::from_millis(16 * i), TouchPhase::Move, x, y));
        }
        steps.push((Duration::from_millis(16 * (n + 1)), TouchPhase::Up, x1, y));
        BackSwipe { start: Instant::now(), steps, next: 0 }
    }

    /// Emit every step now due; returns true once the whole gesture has been sent.
    fn pump(&mut self, tx: &Sender<InputFrame>) -> bool {
        let elapsed = self.start.elapsed();
        while self.next < self.steps.len() && self.steps[self.next].0 <= elapsed {
            let (_, phase, x, y) = self.steps[self.next];
            send_touch(tx, phase, x, y);
            self.next += 1;
        }
        self.next >= self.steps.len()
    }
}

#[derive(Default)]
struct InputState {
    touching: bool,
    last: (f32, f32),
    /// Index of the sidebar button the press started on, while still held.
    sidebar_down: Option<usize>,
    /// Previous-frame down-state of the mapped mouse buttons (right/middle/4th),
    /// so each system action fires once on the press edge, not every frame it's
    /// held. See the rising-edge check in `pump_input`.
    prev_back: bool,
    prev_home: bool,
    prev_recents: bool,
    /// An in-flight right-click back-swipe, animating across frames.
    back_swipe: Option<BackSwipe>,
}

/// Per-frame input collaborators, bundled so `pump_input` and its helpers
/// stay under the 4-parameter line instead of threading five separate
/// references through each call.
struct InputCtx<'a> {
    /// Window points excluding the sidebar; touch coordinates are mapped
    /// against this so the button panel never counts as part of the device
    /// screen.
    content_w: usize,
    win_h: usize,
    tx: &'a Sender<InputFrame>,
    clip: &'a Arc<Mutex<ClipState>>,
}

/// Whether the given virtual-key is currently held, from the high bit of the
/// Win32 async key state. `pump_input` tracks this frame-to-frame and fires on
/// the rising edge, instead of reading the "pressed since last call" low bit —
/// that bit is shared process-wide and gets consumed by any other program that
/// polls GetAsyncKeyState, which made right-click intermittently miss. The high
/// bit is a plain live-state read, so it's reliable. Mouse VKs: 0x02 = right,
/// 0x04 = middle, 0x05 = X1 (4th / back-side button).
#[cfg(windows)]
fn vkey_down(vkey: i32) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }
    unsafe { (GetAsyncKeyState(vkey) as u16 & 0x8000) != 0 }
}
#[cfg(not(windows))]
fn vkey_down(_vkey: i32) -> bool {
    false
}

/// Turn this frame's mouse state into touch messages, or, for a press that
/// starts in the sidebar strip, a button action. Keyboard shortcuts are
/// handled separately by the event monitor.
fn pump_input(window: &mut Window, frame: &DecodedFrame, state: &mut InputState, ctx: &InputCtx) {
    // Mouse buttons mirror scrcpy: right = Back, middle = Home, 4th = App Switcher.
    // Read each button's live down-state (VK_RBUTTON 0x02, VK_MBUTTON 0x04,
    // VK_XBUTTON1 0x05) and fire once on the rising edge, so holding it doesn't
    // repeat the action. Only act while our window has focus so clicks in other
    // apps never reach the phone; still record the state every frame (even when
    // unfocused) so a button pressed elsewhere doesn't fire when focus returns.
    // Advance any in-flight synthetic back-swipe first, and clear it when done.
    if let Some(sw) = state.back_swipe.as_mut() {
        if sw.pump(ctx.tx) {
            state.back_swipe = None;
        }
    }

    let (bd, hd, rd) = (vkey_down(0x02), vkey_down(0x04), vkey_down(0x05));
    if window.is_active() {
        if bd && !state.prev_back {
            send_action(ctx.tx, SystemAction::Back);
            // Also drive the native left-edge swipe, for apps that ignore Cmd+[.
            // Skip if a swipe is already playing or a finger is already down.
            if state.back_swipe.is_none() && !state.touching {
                state.back_swipe = Some(BackSwipe::new());
            }
        }
        if hd && !state.prev_home {
            send_action(ctx.tx, SystemAction::Home);
        }
        if rd && !state.prev_recents {
            send_action(ctx.tx, SystemAction::AppSwitcher);
        }
    }
    state.prev_back = bd;
    state.prev_home = hd;
    state.prev_recents = rd;

    // While the back-swipe animates, don't also drive touch from the mouse — both
    // use finger 0 and would fight. Normal touch resumes once it finishes.
    if state.back_swipe.is_some() {
        return;
    }

    let down = window.get_mouse_down(MouseButton::Left);

    if !down {
        if state.touching {
            // Button released: always lift, using the last known position, even if
            // the cursor is now outside the window (a fast swipe can release out
            // there). Skip this and a phantom finger stays down on the device,
            // which then ignores every later touch until something resets it.
            send_touch(ctx.tx, TouchPhase::Up, state.last.0, state.last.1);
            state.touching = false;
        }
        state.sidebar_down = None;
        return;
    }

    let Some(pos) = window.get_mouse_pos(MouseMode::Clamp) else {
        return;
    };

    if state.touching {
        let (nx, ny) = map_to_norm(pos.0, pos.1, ctx.content_w, ctx.win_h, frame.width, frame.height);
        if (nx - state.last.0).abs() > 0.001 || (ny - state.last.1).abs() > 0.001 {
            // Only emit a move when the position actually changes.
            send_touch(ctx.tx, TouchPhase::Move, nx, ny);
        }
        state.last = (nx, ny);
        return;
    }

    if state.sidebar_down.is_some() {
        return; // press started on a button; ignore drag until release
    }

    handle_fresh_press(pos, frame, state, ctx);
}

/// A press that wasn't already tracked as a touch or a held button: route it
/// to a touch-down or a sidebar button depending on where it landed.
fn handle_fresh_press(pos: (f32, f32), frame: &DecodedFrame, state: &mut InputState, ctx: &InputCtx) {
    let (mx, my) = pos;
    if (mx as usize) < ctx.content_w {
        let (nx, ny) = map_to_norm(mx, my, ctx.content_w, ctx.win_h, frame.width, frame.height);
        send_touch(ctx.tx, TouchPhase::Down, nx, ny);
        state.touching = true;
        state.last = (nx, ny);
    } else if let Some(idx) = sidebar::hit_test(mx - ctx.content_w as f32, my, ctx.win_h as f32) {
        state.sidebar_down = Some(idx);
        dispatch_sidebar_action(ctx.tx, ctx.clip, sidebar::BUTTONS[idx]);
    }
}

fn send_touch(tx: &Sender<InputFrame>, phase: TouchPhase, x: f32, y: f32) {
    let _ = tx.send(InputFrame::new(
        MessageType::InputTouch,
        protocol::encode_touch(phase, 0, x, y),
    ));
}

fn wait_for_first_frame(frames: &FrameSlot, stop: &Arc<AtomicBool>) -> Option<DecodedFrame> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        if let Some(decoded) = frames.lock().unwrap().take() {
            return Some(decoded);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}
