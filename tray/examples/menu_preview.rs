//! The tray's menu, drawn on its own for checking by eye and by script (Issue #78): the same items
//! (`ui::build_menu`) with the same texts (`view::view`) in a chosen language and state, shown as a
//! popup menu of a window of its own. It has **no** tray icon, no server, no supervisor, no settings
//! file, no registry access and no single-instance guard, and its window class is its own (never the
//! tray's `MemocampusTrayHost`), so it can run beside an installed tray without touching it. The
//! tray itself has no option to force a language; this is the only place a language is chosen.
//!
//!   menu_preview <ja|en> <state> <out-dir> [click <position> | key <downs>]
//!
//! `state`: stopped, starting, running, stopping, error-port, error-vault, crash-loop, quitting.
//! Writes `<out-dir>/menu-<lang>-<state>.bmp` (the menu as drawn on screen) and `.txt` (each item:
//! position, id, enabled/grey state, text, darkest pixel), then closes the menu with Escape.
//! `click <position>` clicks that item with the mouse, `key <downs>` presses Down that many times and
//! Enter; either then records which menu id (if any) was delivered as a click.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    std::process::exit(preview::run());
}

#[cfg(windows)]
mod preview {
    use memocampus_tray::autostart::{Approval, Autostart};
    use memocampus_tray::i18n::Lang;
    use memocampus_tray::state::{ErrorKind, ServerState, Snapshot, VaultStatus};
    use memocampus_tray::ui::build_menu;
    use memocampus_tray::view::{view, Inputs, SettingsNote};
    use std::fmt::Write as _;
    use std::path::PathBuf;
    use std::ptr::null_mut;
    use std::time::Duration;
    use tray_icon::menu::{ContextMenu, MenuEvent};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, VK_DOWN,
        VK_ESCAPE, VK_RETURN,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    unsafe extern "system" fn window_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, w, l)
    }

    fn inputs(state: &str) -> Option<Inputs> {
        let mut snapshot = Snapshot::initial();
        let mut quitting = false;
        snapshot.state = match state {
            "stopped" => ServerState::Stopped,
            "starting" => ServerState::Starting,
            "running" => {
                snapshot.port = Some(8765);
                ServerState::Running
            }
            "stopping" => ServerState::Stopping,
            "error-port" => ServerState::Error(ErrorKind::PortInUse),
            "error-vault" => {
                snapshot.vault = VaultStatus::Refused("vault_publication_pending".into());
                ServerState::Error(ErrorKind::VaultRefused("vault_publication_pending".into()))
            }
            "crash-loop" => ServerState::Error(ErrorKind::CrashLoop),
            "quitting" => {
                quitting = true;
                snapshot.port = Some(8765);
                ServerState::Running
            }
            _ => return None,
        };
        Some(Inputs {
            snapshot,
            listen_port: 8765,
            autostart_setting: true,
            server_autostart_setting: true,
            autostart: Autostart::Registered(Approval::Enabled),
            settings_note: SettingsNote::InForce,
            last_error: None,
            quitting,
        })
    }

    /// The open popup menu's window and menu handle.
    fn open_menu() -> Option<(HWND, HMENU)> {
        for _ in 0..200 {
            // SAFETY: looks up the system's popup-menu window class; no handle is kept past its use.
            let window = unsafe { FindWindowW(wide("#32768").as_ptr(), null_mut()) };
            if !window.is_null() {
                // SAFETY: MN_GETHMENU asks a menu window for its menu.
                let menu = unsafe { SendMessageW(window, 0x01E1, 0, 0) } as HMENU;
                if !menu.is_null() {
                    return Some((window, menu));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// Captures the menu window into a 32-bit BMP and describes each item.
    fn capture(window: HWND, menu: HMENU, bmp: &PathBuf) -> String {
        let mut report = String::new();
        // SAFETY: GDI calls on the screen DC and objects created and freed here.
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(window, &mut r);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            SelectObject(dc, bitmap as _);
            BitBlt(dc, 0, 0, w, h, screen, r.left, r.top, SRCCOPY);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            GetDIBits(
                dc,
                bitmap,
                0,
                h as u32,
                pixels.as_mut_ptr() as _,
                &mut info,
                DIB_RGB_COLORS,
            );
            let mut file = Vec::with_capacity(54 + pixels.len());
            file.extend_from_slice(b"BM");
            file.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
            file.extend_from_slice(&[0; 4]);
            file.extend_from_slice(&54u32.to_le_bytes());
            file.extend_from_slice(&40u32.to_le_bytes());
            file.extend_from_slice(&w.to_le_bytes());
            file.extend_from_slice(&h.to_le_bytes());
            file.extend_from_slice(&1u16.to_le_bytes());
            file.extend_from_slice(&32u16.to_le_bytes());
            file.extend_from_slice(&[0; 24]);
            file.extend_from_slice(&pixels);
            let _ = std::fs::write(bmp, file);
            for position in 0..GetMenuItemCount(menu).max(0) as u32 {
                let mut item: MENUITEMINFOW = std::mem::zeroed();
                item.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
                item.fMask = MIIM_STATE | MIIM_FTYPE | MIIM_ID;
                GetMenuItemInfoW(menu, position, 1, &mut item);
                let mut text = [0u16; 256];
                let n = GetMenuStringW(menu, position, text.as_mut_ptr(), 256, MF_BYPOSITION);
                let text = String::from_utf16_lossy(&text[..n.max(0) as usize]);
                let mut rect: RECT = std::mem::zeroed();
                GetMenuItemRect(null_mut(), menu, position, &mut rect);
                let (top, bottom) = ((rect.top - r.top).max(0), (rect.bottom - r.top).min(h));
                let mut darkest = (u32::MAX, [0u8; 3]);
                for y in top..bottom {
                    let row = (h - 1 - y) as usize;
                    for x in 0..w as usize {
                        let p = &pixels[(row * w as usize + x) * 4..][..3];
                        let sum = p.iter().map(|&c| c as u32).sum::<u32>();
                        if sum < darkest.0 {
                            darkest = (sum, [p[2], p[1], p[0]]);
                        }
                    }
                }
                let separator = item.fType & MFT_SEPARATOR != 0;
                let _ = writeln!(
                    report,
                    "pos={position} separator={separator} disabled={} checked={} darkest_rgb={:?} text={text}",
                    item.fState & MFS_DISABLED != 0,
                    item.fState & MFS_CHECKED != 0,
                    darkest.1
                );
            }
            DeleteObject(bitmap as _);
            DeleteDC(dc);
            ReleaseDC(null_mut(), screen);
            let _ = writeln!(
                report,
                "COLOR_MENUTEXT={:06x} COLOR_GRAYTEXT={:06x}",
                GetSysColor(COLOR_MENUTEXT),
                GetSysColor(COLOR_GRAYTEXT)
            );
        }
        report
    }

    /// The positions the keyboard has highlighted now.
    fn highlighted(menu: HMENU) -> Vec<u32> {
        // SAFETY: reads item states of the open popup menu.
        unsafe {
            (0..GetMenuItemCount(menu).max(0) as u32)
                .filter(|&position| GetMenuState(menu, position, MF_BYPOSITION) & MF_HILITE != 0)
                .collect()
        }
    }

    fn key(window: HWND, vk: u16) {
        // SAFETY: a key posted to the popup menu's own window.
        unsafe { PostMessageW(window, WM_KEYDOWN, vk as WPARAM, 0) };
        std::thread::sleep(Duration::from_millis(150));
    }

    fn click(menu: HMENU, position: u32) {
        // SAFETY: moves the cursor onto the item of our own popup menu and clicks there.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetMenuItemRect(null_mut(), menu, position, &mut rect);
            SetCursorPos((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
            std::thread::sleep(Duration::from_millis(200));
            let mut input: [INPUT; 2] = std::mem::zeroed();
            input[0].r#type = INPUT_MOUSE;
            input[0].Anonymous.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;
            input[1].r#type = INPUT_MOUSE;
            input[1].Anonymous.mi.dwFlags = MOUSEEVENTF_LEFTUP;
            SendInput(2, input.as_ptr(), std::mem::size_of::<INPUT>() as i32);
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let lang = match args.first().map(String::as_str) {
            Some("ja") => Lang::Ja,
            Some("en") => Lang::En,
            _ => return 2,
        };
        let Some(state) = args.get(1) else { return 2 };
        let Some(inputs) = inputs(state) else {
            return 2;
        };
        let Some(out) = args.get(2).map(PathBuf::from) else {
            return 2;
        };
        let action = (
            args.get(3).cloned(),
            args.get(4).and_then(|n| n.parse::<u32>().ok()),
        );
        let name = format!("menu-{}-{state}", lang.code());
        let (bmp, txt) = (
            out.join(format!("{name}.bmp")),
            out.join(format!("{name}.txt")),
        );

        // SAFETY: a hidden window of a class of its own, on this thread, for the popup menu.
        let window = unsafe {
            let instance = GetModuleHandleW(null_mut());
            let class = wide("MemocampusMenuPreview");
            let mut description: WNDCLASSEXW = std::mem::zeroed();
            description.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            description.lpfnWndProc = Some(window_proc);
            description.hInstance = instance;
            description.lpszClassName = class.as_ptr();
            RegisterClassExW(&description);
            CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Memocampus menu preview").as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            )
        };
        let Ok((menu, items)) = build_menu(lang) else {
            return 3;
        };
        items.apply(&view(&inputs, lang));

        let worker = std::thread::spawn(move || {
            let Some((popup, hmenu)) = open_menu() else {
                return "no menu window\n".to_owned();
            };
            std::thread::sleep(Duration::from_millis(500));
            let mut report = capture(popup, hmenu, &bmp);
            match action {
                (Some(kind), Some(n)) if kind == "click" => {
                    let _ = writeln!(report, "action=click position={n}");
                    click(hmenu, n);
                }
                (Some(kind), Some(n)) if kind == "key" => {
                    let _ = writeln!(report, "action=key downs={n}");
                    for _ in 0..n {
                        key(popup, VK_DOWN);
                    }
                    let _ = writeln!(report, "highlighted_before_enter={:?}", highlighted(hmenu));
                    key(popup, VK_RETURN);
                }
                _ => {}
            }
            // Whatever is still open is closed without choosing anything.
            // SAFETY: only reads whether the menu window still exists.
            if unsafe { IsWindow(popup) } != 0 {
                key(popup, VK_ESCAPE);
            }
            report
        });
        // SAFETY: shows the menu for our own window; returns once it is closed.
        unsafe {
            SetForegroundWindow(window);
            menu.show_context_menu_for_hwnd(window as isize, None);
        }
        let mut report = worker.join().unwrap_or_default();
        let mut delivered = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            delivered.push(event.id().0.clone());
        }
        let _ = writeln!(report, "delivered={delivered:?}");
        let _ = std::fs::write(&txt, &report);
        // SAFETY: our own window.
        unsafe { DestroyWindow(window) };
        0
    }
}
