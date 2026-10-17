//! Build tool only. The installed application does not ship or invoke this tool.
use std::{io, path::Path};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::FreeLibrary, System::LibraryLoader::*, UI::WindowsAndMessaging::RT_MANIFEST,
    },
};

const MANIFEST: &[u8] = include_bytes!("../../desktop/app.manifest");
fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn patch(path: &Path) -> io::Result<()> {
    let filename = wide(path);
    unsafe {
        let handle =
            BeginUpdateResourceW(PCWSTR(filename.as_ptr()), false).map_err(io::Error::other)?;
        let update = (|| -> windows::core::Result<()> {
            UpdateResourceW(
                handle,
                RT_MANIFEST,
                PCWSTR(std::ptr::dangling::<u16>()),
                0x409,
                Some(MANIFEST.as_ptr().cast()),
                MANIFEST.len() as u32,
            )?;
            UpdateResourceW(
                handle,
                RT_MANIFEST,
                PCWSTR(std::ptr::dangling::<u16>()),
                0x409,
                None,
                0,
            )
        })();
        if let Err(e) = update {
            let _ = EndUpdateResourceW(handle, true);
            return Err(io::Error::other(e));
        }
        EndUpdateResourceW(handle, false).map_err(io::Error::other)?;
    }
    verify(path)
}
fn verify(path: &Path) -> io::Result<()> {
    let filename = wide(path);
    unsafe {
        let module = LoadLibraryExW(
            PCWSTR(filename.as_ptr()),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
        .map_err(io::Error::other)?;
        let result = (|| {
            let resource = FindResourceW(
                Some(module),
                PCWSTR(std::ptr::dangling::<u16>()),
                RT_MANIFEST,
            );
            if resource.is_invalid() {
                return Err(io::Error::other("Final application manifest is missing"));
            }
            let count = SizeofResource(Some(module), resource) as usize;
            let data = LoadResource(Some(module), resource).map_err(io::Error::other)?;
            let ptr = LockResource(data) as *const u8;
            if ptr.is_null()
                || count != MANIFEST.len()
                || std::slice::from_raw_parts(ptr, count) != MANIFEST
            {
                return Err(io::Error::other(
                    "Final application manifest does not match the source",
                ));
            }
            if !FindResourceW(
                Some(module),
                PCWSTR(std::ptr::dangling::<u16>()),
                RT_MANIFEST,
            )
            .is_invalid()
            {
                return Err(io::Error::other("Staging manifest was not removed"));
            }
            Ok(())
        })();
        let _ = FreeLibrary(module);
        result
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [action, path] if action == "patch" => patch(Path::new(path)),
        [action, path] if action == "verify" => verify(Path::new(path)),
        _ => Err(io::Error::other(
            "Usage: monarch-package <patch|verify> <application.exe>",
        )),
    };
    if let Err(e) = result {
        eprintln!("Packaging failed: {e}");
        std::process::exit(1);
    }
    println!("Application manifest verified.");
}
