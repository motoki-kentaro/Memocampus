//! The built host executable carries `resources/memocampus.ico` as its icon (see `build.rs`).
#![cfg(all(windows, target_env = "msvc"))]

use std::{os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LoadLibraryExW, LoadResource, LockResource, SizeofResource,
    LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON};

/// Reads resource `id` of `kind` from a module loaded as data; `None` if it does not exist.
fn resource(module: *mut core::ffi::c_void, kind: *const u16, id: u16) -> Option<Vec<u8>> {
    unsafe {
        let info = FindResourceW(module, id as usize as *const u16, kind);
        if info.is_null() {
            return None;
        }
        let data = LockResource(LoadResource(module, info)) as *const u8;
        assert!(!data.is_null());
        Some(std::slice::from_raw_parts(data, SizeofResource(module, info) as usize).to_vec())
    }
}

#[test]
fn the_host_executable_carries_the_memocampus_icon() {
    let ico = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/memocampus.ico"))
        .unwrap();
    let u32_at = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let sizes: Vec<u8> = (0..count).map(|index| ico[6 + 16 * index]).collect();
    assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 0], "0 stands for 256 px");

    let exe: Vec<u16> = Path::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let flags = LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE;
    let module = unsafe { LoadLibraryExW(exe.as_ptr(), std::ptr::null_mut(), flags) };
    assert!(!module.is_null(), "load the executable as data");

    let group = resource(module, RT_GROUP_ICON, 1).expect("icon group 1");
    assert_eq!(&group[..6], &ico[..6], "same header and image count");
    for index in 0..count {
        let entry = &group[6 + 14 * index..6 + 14 * (index + 1)];
        assert_eq!(&entry[..12], &ico[6 + 16 * index..6 + 16 * index + 12]);
        let id = u16::from_le_bytes([entry[12], entry[13]]);
        let (size, offset) = (u32_at(6 + 16 * index + 8), u32_at(6 + 16 * index + 12));
        let image = resource(module, RT_ICON, id).expect("icon image");
        assert!(
            image == ico[offset..offset + size],
            "image {index} is the ICO's, byte for byte"
        );
    }
    unsafe { FreeLibrary(module) };
}
