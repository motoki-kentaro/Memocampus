//! Native file/folder pickers (rfd). The extension cannot obtain Windows paths itself, so the
//! host shows the OS dialog. Abstracted behind a trait so message handling is testable.

use std::path::{Path, PathBuf};

pub trait Dialogs {
    fn choose_new_vault(&self, default_dir: &Path, default_name: &str) -> Option<PathBuf>;
    fn choose_existing_vault(&self, start_dir: Option<&Path>) -> Option<PathBuf>;
    fn choose_backup_dir(&self) -> Option<PathBuf>;
}

pub struct NativeDialogs;

const VAULT_EXTENSIONS: [&str; 3] = ["sqlite3", "sqlite", "db"];

impl Dialogs for NativeDialogs {
    fn choose_new_vault(&self, default_dir: &Path, default_name: &str) -> Option<PathBuf> {
        let owner = owner::OwnerWindow::create();
        let dialog = rfd::FileDialog::new()
            .set_title("新しいMemocampus Vaultの保存先")
            .add_filter("Memocampus Vault", &VAULT_EXTENSIONS)
            .set_directory(default_dir)
            .set_file_name(default_name);
        owner::with_parent(dialog, owner.as_ref()).save_file()
    }

    fn choose_existing_vault(&self, start_dir: Option<&Path>) -> Option<PathBuf> {
        let owner = owner::OwnerWindow::create();
        let mut dialog = rfd::FileDialog::new()
            .set_title("既存のMemocampus Vaultを開く")
            .add_filter("Memocampus Vault", &VAULT_EXTENSIONS);
        if let Some(dir) = start_dir.filter(|dir| dir.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        owner::with_parent(dialog, owner.as_ref()).pick_file()
    }

    fn choose_backup_dir(&self) -> Option<PathBuf> {
        let owner = owner::OwnerWindow::create();
        let dialog = rfd::FileDialog::new().set_title("バックアップの保存先フォルダ");
        owner::with_parent(dialog, owner.as_ref()).pick_folder()
    }
}

#[cfg(windows)]
mod owner {
    //! The host runs in the background, so a bare dialog may open behind Chrome. A hidden,
    //! topmost window owned by this process is used as the dialog owner. Chrome's own window is
    //! deliberately not used: a cross-process modal owner would stay disabled if the host died.

    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
        Win32WindowHandle, WindowHandle,
    };
    use std::num::NonZeroIsize;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, SetForegroundWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_POPUP,
    };

    pub struct OwnerWindow(HWND);

    impl OwnerWindow {
        pub fn create() -> Option<Self> {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let title: Vec<u16> = "Memocampus\0".encode_utf16().collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            if hwnd.is_null() {
                return None;
            }
            // Best effort; Windows may refuse foreground activation for a background process.
            unsafe { SetForegroundWindow(hwnd) };
            Some(Self(hwnd))
        }
    }

    impl Drop for OwnerWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    impl HasWindowHandle for OwnerWindow {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let hwnd = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
            // SAFETY: the window lives until `self` is dropped, which outlives the dialog call.
            Ok(unsafe {
                WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)))
            })
        }
    }

    impl HasDisplayHandle for OwnerWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }

    pub fn with_parent(dialog: rfd::FileDialog, owner: Option<&OwnerWindow>) -> rfd::FileDialog {
        match owner {
            Some(owner) => dialog.set_parent(owner),
            None => dialog,
        }
    }
}

#[cfg(not(windows))]
mod owner {
    pub struct OwnerWindow;

    impl OwnerWindow {
        pub fn create() -> Option<Self> {
            None
        }
    }

    pub fn with_parent(dialog: rfd::FileDialog, _owner: Option<&OwnerWindow>) -> rfd::FileDialog {
        dialog
    }
}

#[cfg(all(test, windows))]
mod tests {
    use raw_window_handle::HasWindowHandle;

    #[test]
    fn owner_window_is_created_and_exposes_a_win32_handle() {
        // Creates and destroys the hidden owner only; no dialog is shown.
        let owner = super::owner::OwnerWindow::create().expect("owner window");
        assert!(owner.window_handle().is_ok());
    }
}
