//! Windows keeps the verified file handle exclusive until the rename finishes.
//! This avoids closing the handle and then renaming a possibly different path.
use std::{ffi::c_void, fs::{File, OpenOptions}, os::windows::{ffi::OsStrExt, fs::{MetadataExt, OpenOptionsExt}, io::AsRawHandle}, path::Path};

#[repr(C)]
#[derive(Default)]
struct FileInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[repr(C)]
struct RenameInformation {
    replace_or_flags: u32,
    root: *mut c_void,
    name_bytes: u32,
    name: [u16; 1],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(file: *mut c_void, info: *mut FileInformation) -> i32;
    fn SetFileInformationByHandle(file: *mut c_void, class: i32, info: *const c_void, bytes: u32) -> i32;
}

pub(super) fn open(path: &Path, exclusive: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    // OPEN_REPARSE_POINT: inspect the link itself, never its target.
    options.read(true).custom_flags(0x00200000);
    if exclusive { options.access_mode(0x80000000 | 0x00010000).share_mode(0); } // GENERIC_READ | DELETE
    let file = options.open(path).map_err(|e| format!("Cannot acquire exclusive access to index.lock. A process may still use it, or access is denied. Nothing was moved. {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.file_attributes() & 0x400 != 0 || !meta.is_file() { return Err("The index lock is a reparse point or not a regular file. Nothing was changed.".into()); }
    Ok(file)
}

pub(super) fn identity(file: &File) -> Result<(u64, u128), String> {
    let mut info = FileInformation::default();
    // SAFETY: live File handle and a correctly laid out writable output struct.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(format!("Cannot verify the lock's file identity: {}", std::io::Error::last_os_error()));
    }
    if info.links != 1 { return Err("The index lock has multiple hard links. Recovery was refused.".into()); }
    Ok((info.volume as u64, ((info.index_high as u128) << 32) | info.index_low as u128))
}

pub(super) fn move_to_backup(file: &File, destination: &Path) -> Result<(), String> {
    let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let offset = std::mem::offset_of!(RenameInformation, name);
    let bytes = (offset + (name.len() + 1) * 2).max(std::mem::size_of::<RenameInformation>());
    let length = u32::try_from(bytes).map_err(|_| "Backup path is too long")?;
    // usize storage provides the pointer alignment required by FILE_RENAME_INFO.
    let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<RenameInformation>();
    // SAFETY: aligned zero-initialized buffer with enough room for header, UTF-16
    // path and null terminator. FileRenameInfo=3; ReplaceIfExists remains false.
    let ok = unsafe {
        (*info).name_bytes = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), buffer.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>(), name.len());
        SetFileInformationByHandle(file.as_raw_handle(), 3, info.cast(), length)
    };
    if ok == 0 { return Err(format!("Cannot move the index lock to its backup. Nothing was deleted. {}", std::io::Error::last_os_error())); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn held_windows_handle_blocks_recovery_and_rename_preserves_bytes() {
        let dir = std::env::temp_dir().join(format!("gdd-lock-handle-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("index.lock");
        std::fs::write(&path, b"unfinished index").unwrap();
        let other = File::open(&path).unwrap();
        assert!(open(&path, true).is_err());
        drop(other);
        let file = open(&path, true).unwrap();
        assert!(File::open(&path).is_err());
        let backup = dir.join("backup");
        move_to_backup(&file, &backup).unwrap();
        drop(file);
        assert!(!path.exists());
        assert_eq!(std::fs::read(&backup).unwrap(), b"unfinished index");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
