//! What kind of drive a rip is on: whether reading it costs a seek (a hard
//! drive) or not (an SSD), as Windows says. Two rips read at once from a hard
//! drive take longer than one after the other; from an SSD, less. And how
//! much room a drive has left, for Telly's copies of episodes.

use std::path::Path;

/// Whether the drive whose root is `root` ("D:\") moves a head to read: a
/// hard drive. None when that can't be told: a network share, or a USB
/// enclosure that doesn't pass the question on.
#[cfg(windows)]
pub fn seek_penalty(root: &Path) -> Option<bool> {
    windows::seek_penalty(&volume_of(root)?)
}

/// Elsewhere than Windows (where Telly is only developed) it isn't told:
/// rips are read as from a hard drive.
#[cfg(not(windows))]
pub fn seek_penalty(_root: &Path) -> Option<bool> {
    None
}

/// How many bytes can still be written in `folder`, which is there. None
/// when Windows won't say.
#[cfg(windows)]
pub fn free_space(folder: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let name: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call, and `free` a u64 to write to.
    let asked = unsafe { GetDiskFreeSpaceExW(name.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (asked != 0).then_some(free)
}

/// Elsewhere than Windows (where Telly is only developed) it isn't told.
#[cfg(not(windows))]
pub fn free_space(_folder: &Path) -> Option<u64> {
    None
}

/// The device Windows opens to ask about the drive whose root is `root`
/// ("D:\" → "\\.\D:"). None for a network share.
#[cfg_attr(not(windows), allow(dead_code))]
fn volume_of(root: &Path) -> Option<String> {
    let root = root.to_str()?;
    let root = root.strip_prefix(r"\\?\").unwrap_or(root);
    let letter = root.strip_suffix('\\').unwrap_or(root).strip_suffix(':')?;
    (letter.len() == 1 && letter.chars().all(|c| c.is_ascii_alphabetic())).then(|| format!(r"\\.\{letter}:"))
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };

    /// Asks the storage driver of `volume` ("\\.\D:") whether it incurs a
    /// seek penalty (IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceSeekPenaltyProperty),
    /// as Windows' own defragmenter does. Opening a volume to ask needs no
    /// read or write access, so no administrator.
    pub fn seek_penalty(volume: &str) -> Option<bool> {
        let name: Vec<u16> = volume.encode_utf16().chain([0]).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call;
        // the handle is checked, used for one query, and closed.
        unsafe {
            let handle = CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let query = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceSeekPenaltyProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
            let mut answer = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
            let mut returned = 0u32;
            let asked = DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&raw const query).cast::<c_void>(),
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                (&raw mut answer).cast::<c_void>(),
                size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(handle);
            (asked != 0 && returned as usize >= size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()).then_some(answer.IncursSeekPenalty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_letter_is_asked_about_as_its_volume() {
        assert_eq!(volume_of(Path::new("D:\\")).as_deref(), Some(r"\\.\D:"));
        assert_eq!(volume_of(Path::new(r"\\?\E:\")).as_deref(), Some(r"\\.\E:"));
        // A network share, or a path of another system, isn't.
        assert_eq!(volume_of(Path::new(r"\\nas\anime\")), None);
        assert_eq!(volume_of(Path::new("/")), None);
    }
}
