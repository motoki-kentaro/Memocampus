//! Embeds `resources/memocampus.ico` as the executable's icon on Windows (MSVC).
//!
//! The ICO is turned into a compiled resource file (`.res`) here, without `rc.exe` or another
//! crate, and handed to the MSVC linker, which accepts `.res` inputs directly. The ICO itself is
//! generated from `assets/icon.png` by `scripts/generate-icons.py` and committed.

use std::{env, fs, path::PathBuf};

const ICON: &str = "resources/memocampus.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// Resource id of the icon group; Explorer shows the first group.
const GROUP_ID: u16 = 1;

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed=build.rs");
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows {
        return;
    }
    if !msvc {
        println!("cargo:warning=the executable icon is only embedded with the MSVC toolchain");
        return;
    }
    let ico = fs::read(ICON).unwrap_or_else(|error| panic!("{ICON}: {error}"));
    let res = PathBuf::from(env::var("OUT_DIR").unwrap()).join("memocampus-icon.res");
    fs::write(&res, icon_resources(&ico)).unwrap();
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// One ICO directory entry: the 12 header bytes Windows keeps in the group, and the image data.
struct Entry<'a> {
    header: &'a [u8],
    data: &'a [u8],
}

fn entries(ico: &[u8]) -> Vec<Entry<'_>> {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap()) as usize;
    assert!(
        ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1,
        "{ICON} is not an icon file"
    );
    let count = u16_at(4) as usize;
    assert!(
        count > 0 && ico.len() >= 6 + 16 * count,
        "{ICON} is truncated"
    );
    (0..count)
        .map(|index| {
            let at = 6 + 16 * index;
            let (size, offset) = (u32_at(at + 8), u32_at(at + 12));
            assert!(
                offset + size <= ico.len(),
                "{ICON}: entry {index} is truncated"
            );
            Entry {
                header: &ico[at..at + 12],
                data: &ico[offset..offset + size],
            }
        })
        .collect()
}

/// A `.res` file: the empty leading entry, one `RT_ICON` per image (ids 1..), one `RT_GROUP_ICON`.
fn icon_resources(ico: &[u8]) -> Vec<u8> {
    let entries = entries(ico);
    let mut res = Vec::new();
    resource(&mut res, 0, 0, 0, &[]);
    let mut group = vec![0, 0, 1, 0];
    group.extend((entries.len() as u16).to_le_bytes());
    for (index, entry) in entries.iter().enumerate() {
        let id = index as u16 + 1;
        resource(&mut res, RT_ICON, id, 0x1010, entry.data);
        group.extend(entry.header);
        group.extend(id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, GROUP_ID, 0x1030, &group);
    res
}

fn resource(res: &mut Vec<u8>, kind: u16, id: u16, memory_flags: u16, data: &[u8]) {
    res.extend((data.len() as u32).to_le_bytes());
    res.extend(32u32.to_le_bytes()); // header size
    res.extend([0xFF, 0xFF]);
    res.extend(kind.to_le_bytes());
    res.extend([0xFF, 0xFF]);
    res.extend(id.to_le_bytes());
    res.extend(0u32.to_le_bytes()); // data version
    res.extend(memory_flags.to_le_bytes());
    res.extend(0u16.to_le_bytes()); // language neutral
    res.extend(0u32.to_le_bytes()); // version
    res.extend(0u32.to_le_bytes()); // characteristics
    res.extend(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
