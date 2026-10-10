//! Windows half of the `imp` backend declared in `super`.
//!
//! This owns every operation whose safety story depends on a Windows
//! object-handle primitive: `NtCreateFile` against an already-authorized
//! parent directory handle with reparse-point parsing disabled, a protected
//! current-user-only DACL on private objects, and the pin/rename-aside/
//! no-replace-rename sequence that stands in for the atomic name exchange
//! the Unix backend gets from `renameat2`.
//!
//! It is a separate file because it is the only place in the crate that
//! names a `windows-sys` WDK item, and because the sibling `imp_unix.rs`
//! and `imp_other.rs` backends implement the same private interface with an
//! entirely different syscall vocabulary. Keeping the platform vocabularies
//! in sibling files means neither has to be cfg-pruned out of the other.

use super::*;
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::{Component, PathBuf, Prefix};
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_CREATE, FILE_DIRECTORY_FILE,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, RtlNtStatusToDosError, ERROR_SHARING_VIOLATION, HANDLE,
    INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, SET_ACCESS,
    SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetTokenInformation, InitializeSecurityDescriptor, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TokenUser, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    INHERITED_ACE, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_DESCRIPTOR,
    SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx, FileNameInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, SetFileInformationByHandle,
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA, FILE_ATTRIBUTE_ARCHIVE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SYSTEM, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_LIST_DIRECTORY, FILE_NAME_INFO, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const BASIC_DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const PRIVATE_DIRECTORY_INSPECTION_ACCESS: u32 = BASIC_DIRECTORY_ACCESS | READ_CONTROL | WRITE_DAC;
const PRIVATE_DIRECTORY_CREATE_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | WRITE_DAC;
const PRIVATE_INSPECTION_ACCESS: u32 =
    FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC | SYNCHRONIZE;
const PRIVATE_FILE_ACCESS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | WRITE_DAC;
// Session tail recovery truncates torn records before later appends, which
// requires FILE_WRITE_DATA on Windows. Callers seek to the durable end
// before writing; Session additionally repeats that seek under its lock.
const APPEND_ACCESS: u32 = FILE_READ_DATA
    | FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | READ_CONTROL
    | SYNCHRONIZE;
// A pin denies both write and delete sharing. Existing writable handles,
// even cooperative ones, must close before replacement: otherwise they could
// write after the final comparison and silently lose their update. The pin
// retains the verified object through displacement and publication.
const PIN_ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE;
const PIN_SHARE: u32 = FILE_SHARE_READ;
// Scanners and sync clients briefly hold files open; retry a pin that
// meets one for up to about 200 ms before reporting the file as in use.
const PIN_SHARING_ATTEMPTS: u32 = 10;
const PIN_SHARING_BACKOFF: std::time::Duration = std::time::Duration::from_millis(20);
// Attributes a replacement carries over from the file it replaces.
const PRESERVED_ATTRIBUTES: u32 = FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
    links: u32,
    attributes: u32,
    size: u64,
    creation_time: u64,
    last_write_time: u64,
}

#[derive(Clone, Copy)]
pub(super) struct PrivateDirectoryIdentity {
    volume: u32,
    index: u64,
}

impl FileIdentity {
    fn is_directory(self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    fn is_reparse_point(self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: this type exclusively owns the successful token handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Security APIs documented to allocate these buffers with LocalAlloc.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct PrivateSecurityDescriptor {
    descriptor: SECURITY_DESCRIPTOR,
    _acl: LocalAllocation,
}

fn invalid_path(path: &Path) -> SecureFileError {
    SecureFileError::InvalidPath(path.display().to_string())
}

fn private_error(message: &str) -> SecureFileError {
    SecureFileError::InsecurePrivateObject(message.to_owned())
}

fn win32_error(code: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code as i32)
}

fn last_error() -> std::io::Error {
    // SAFETY: GetLastError has no preconditions.
    win32_error(unsafe { GetLastError() })
}

fn ntstatus_error(status: i32) -> std::io::Error {
    // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS value.
    win32_error(unsafe { RtlNtStatusToDosError(status) })
}

fn with_current_user_sid<T>(
    operation: impl FnOnce(PSID) -> Result<T, SecureFileError>,
) -> Result<T, SecureFileError> {
    let mut token = null_mut();
    // SAFETY: token is writable and GetCurrentProcess returns a pseudo-handle valid here.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error().into());
    }
    let _token = TokenHandle(token);
    let mut required = 0_u32;
    // SAFETY: a null/zero query is the documented way to obtain the required size.
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(last_error().into());
    }
    let words = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: buffer contains at least required writable bytes and remains alive for operation.
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(last_error().into());
    }
    // SAFETY: successful TokenUser initializes a TOKEN_USER at the aligned buffer start.
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    operation(user.User.Sid)
}

fn build_private_descriptor(
    sid: PSID,
    directory: bool,
) -> Result<PrivateSecurityDescriptor, SecureFileError> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if directory {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
            ..Default::default()
        },
    };
    let mut acl: *mut ACL = null_mut();
    // SAFETY: entry and output pointers are valid; no old ACL is supplied.
    let status = unsafe { SetEntriesInAclW(1, &entry, null(), &mut acl) };
    if status != 0 {
        return Err(win32_error(status).into());
    }
    let acl = LocalAllocation(acl.cast());
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor_pointer = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
    // SAFETY: descriptor is writable and revision 1 is the supported SECURITY_DESCRIPTOR ABI.
    if unsafe { InitializeSecurityDescriptor(descriptor_pointer, 1) } == 0 {
        return Err(last_error().into());
    }
    // SAFETY: sid and ACL remain valid through NtCreateFile; descriptor is initialized.
    if unsafe { SetSecurityDescriptorOwner(descriptor_pointer, sid, 0) } == 0
        || unsafe { SetSecurityDescriptorDacl(descriptor_pointer, 1, acl.0.cast(), 0) } == 0
        || unsafe {
            SetSecurityDescriptorControl(descriptor_pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
        } == 0
    {
        return Err(last_error().into());
    }
    Ok(PrivateSecurityDescriptor {
        descriptor,
        _acl: acl,
    })
}

fn with_private_descriptor<T>(
    directory: bool,
    operation: impl FnOnce(*const SECURITY_DESCRIPTOR) -> Result<T, SecureFileError>,
) -> Result<T, SecureFileError> {
    with_current_user_sid(|sid| {
        let descriptor = build_private_descriptor(sid, directory)?;
        operation(&descriptor.descriptor)
    })
}

fn component_is_safe(name: &OsStr) -> bool {
    let units = name.encode_wide().collect::<Vec<_>>();
    if units.is_empty()
        || units
            .last()
            .is_some_and(|unit| *unit == u16::from(b'.') || *unit == u16::from(b' '))
        || units.iter().any(|unit| {
            *unit == 0 || *unit < 32 || matches!(*unit, 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124)
        })
    {
        return false;
    }
    let text = name.to_string_lossy();
    let stem = text.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches(['.', ' ']).to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

fn split_absolute(path: &Path) -> Result<(PathBuf, Vec<OsString>), SecureFileError> {
    validate_absolute_file_path(path)?;
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(invalid_path(path));
    };
    match prefix.kind() {
        Prefix::Disk(_)
        | Prefix::UNC(_, _)
        | Prefix::VerbatimDisk(_)
        | Prefix::VerbatimUNC(_, _) => {}
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return Err(invalid_path(path)),
    }
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(invalid_path(path));
    }
    let mut root = prefix.as_os_str().to_os_string();
    root.push("\\");
    let mut names = Vec::new();
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid_path(path));
        };
        if !component_is_safe(name) {
            return Err(invalid_path(path));
        }
        names.push(name.to_os_string());
    }
    Ok((PathBuf::from(root), names))
}

fn open_root(path: &Path) -> Result<File, SecureFileError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(BASIC_DIRECTORY_ACCESS)
        .share_mode(SHARE_ALL)
        .custom_flags(
            windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
        );
    let file = options.open(path)?;
    let identity = file_identity(&file)?;
    if !identity.is_directory() || identity.is_reparse_point() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "path component is not a directory",
        )
        .into());
    }
    Ok(file)
}

fn nt_open_at(
    parent: HANDLE,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    attributes: u32,
    security_descriptor: *const SECURITY_DESCRIPTOR,
) -> Result<(File, usize), SecureFileError> {
    nt_open_at_shared(
        parent,
        name,
        access,
        disposition,
        options,
        attributes,
        security_descriptor,
        SHARE_ALL,
    )
}

#[allow(clippy::too_many_arguments)]
fn nt_open_at_shared(
    parent: HANDLE,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    attributes: u32,
    security_descriptor: *const SECURITY_DESCRIPTOR,
    share: u32,
) -> Result<(File, usize), SecureFileError> {
    if !component_is_safe(name) {
        return Err(SecureFileError::InvalidPath(
            name.to_string_lossy().into_owned(),
        ));
    }
    let mut wide = name.encode_wide().collect::<Vec<_>>();
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| SecureFileError::InvalidPath(name.to_string_lossy().into_owned()))?;
    let mut unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent,
        ObjectName: &mut unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: security_descriptor,
        SecurityQualityOfService: null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: all pointers reference initialized storage for the duration of the synchronous call.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object,
            &mut io_status,
            null(),
            attributes,
            share,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(ntstatus_error(status).into());
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::other("NtCreateFile returned an invalid handle").into());
    }
    // SAFETY: successful NtCreateFile transfers one owned handle to this File.
    let file = unsafe { File::from_raw_handle(handle) };
    Ok((file, io_status.Information))
}

fn open_directory_at(
    parent: &File,
    name: &OsStr,
    create: bool,
    private_access: bool,
) -> Result<(File, bool), SecureFileError> {
    let access = if create {
        PRIVATE_DIRECTORY_CREATE_ACCESS
    } else if private_access {
        PRIVATE_DIRECTORY_INSPECTION_ACCESS
    } else {
        BASIC_DIRECTORY_ACCESS
    };
    let disposition = if create { FILE_OPEN_IF } else { FILE_OPEN };
    let open = |descriptor| {
        nt_open_at(
            parent.as_raw_handle(),
            name,
            access,
            disposition,
            FILE_DIRECTORY_FILE,
            FILE_ATTRIBUTE_DIRECTORY,
            descriptor,
        )
    };
    let (file, information) = if create {
        with_private_descriptor(true, open)?
    } else {
        open(null())?
    };
    let identity = file_identity(&file)?;
    if !identity.is_directory() || identity.is_reparse_point() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "path component is not a directory",
        )
        .into());
    }
    Ok((file, information == 2))
}

/// Newly-created directory handles retained until a path walk succeeds.
/// Handle deletion is object-bound, so a name replacement cannot cause
/// rollback to delete the replacement.
struct CreatedDirectories {
    entries: Vec<CreatedDirectory>,
}

struct CreatedDirectory {
    directory: File,
    identity: FileIdentity,
}

impl CreatedDirectories {
    fn record(&mut self, directory: &File) -> Result<(), SecureFileError> {
        let identity = file_identity(directory)?;
        if !identity.is_directory() || identity.is_reparse_point() {
            return Err(SecureFileError::NotRegular);
        }
        self.entries.push(CreatedDirectory {
            directory: directory.try_clone()?,
            identity,
        });
        Ok(())
    }

    fn disarm(mut self) {
        self.entries.clear();
    }

    fn rollback(&mut self) {
        while let Some(created) = self.entries.pop() {
            if file_identity(&created.directory).is_ok_and(|identity| identity == created.identity)
                && created.identity.is_directory()
                && !created.identity.is_reparse_point()
            {
                let _ = delete_handle(&created.directory);
            }
        }
    }
}

impl Drop for CreatedDirectories {
    fn drop(&mut self) {
        self.rollback();
    }
}

fn open_parent(path: &Path, create: bool) -> Result<(File, OsString), SecureFileError> {
    let mut created = CreatedDirectories {
        entries: Vec::new(),
    };
    let result = (|| {
        let (root_path, mut names) = split_absolute(path)?;
        let name = names.pop().ok_or_else(|| invalid_path(path))?;
        let mut directory = open_root(&root_path)?;
        for component in names {
            match open_directory_at(&directory, &component, false, false) {
                Ok((next, _)) => directory = next,
                Err(SecureFileError::Io(error))
                    if create && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    let (next, was_created) =
                        open_directory_at(&directory, &component, true, true)?;
                    if was_created {
                        created.record(&next)?;
                        make_private_acl(&next, true)?;
                    }
                    directory = next;
                }
                Err(error) => return Err(error),
            }
        }
        Ok((directory, name))
    })();
    match result {
        Ok(parent) => {
            created.disarm();
            Ok(parent)
        }
        Err(error) => {
            created.rollback();
            Err(error)
        }
    }
}

fn file_identity(file: &File) -> Result<FileIdentity, SecureFileError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: information is writable and file owns a valid handle.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(last_error().into());
    }
    Ok(FileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        links: information.nNumberOfLinks,
        attributes: information.dwFileAttributes,
        size: (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow),
        creation_time: (u64::from(information.ftCreationTime.dwHighDateTime) << 32)
            | u64::from(information.ftCreationTime.dwLowDateTime),
        last_write_time: (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(information.ftLastWriteTime.dwLowDateTime),
    })
}

fn private_directory_identity(
    directory: &File,
) -> Result<PrivateDirectoryIdentity, SecureFileError> {
    validate_private_acl(directory, true)?;
    let identity = file_identity(directory)?;
    Ok(PrivateDirectoryIdentity {
        volume: identity.volume,
        index: identity.index,
    })
}

fn validate_bound_directory(
    directory: &File,
    expected: &PrivateDirectoryIdentity,
) -> Result<(), SecureFileError> {
    let actual = private_directory_identity(directory)?;
    if (actual.volume, actual.index) != (expected.volume, expected.index) {
        return Err(SecureFileError::Changed);
    }
    Ok(())
}

fn ensure_regular(identity: FileIdentity) -> Result<(), SecureFileError> {
    if identity.is_directory() || identity.is_reparse_point() {
        return Err(SecureFileError::NotRegular);
    }
    Ok(())
}

fn validate_private_acl(file: &File, directory: bool) -> Result<(), SecureFileError> {
    let identity = file_identity(file)?;
    if identity.is_reparse_point()
        || identity.is_directory() != directory
        || (!directory && identity.links != 1)
    {
        return Err(private_error("private object identity is unsafe"));
    }
    with_current_user_sid(|current_sid| {
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: output pointers are writable and file owns a READ_CONTROL handle.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(win32_error(status).into());
        }
        let _descriptor = LocalAllocation(descriptor.cast());
        if owner.is_null() || unsafe { EqualSid(owner, current_sid) } == 0 {
            return Err(private_error(
                "private object is not owned by the current user",
            ));
        }
        if dacl.is_null() {
            return Err(private_error("private object has an unrestricted ACL"));
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: descriptor is a valid descriptor returned by GetSecurityInfo.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(private_error("private object ACL inherits access"));
        }
        let mut acl_info = ACL_SIZE_INFORMATION::default();
        // SAFETY: dacl and output buffer are valid for this query.
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut acl_info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
            || acl_info.AceCount != 1
        {
            return Err(private_error("private object ACL is not owner-only"));
        }
        let mut ace_pointer: *mut c_void = null_mut();
        // SAFETY: a one-entry ACL guarantees index zero exists when GetAce succeeds.
        if unsafe { GetAce(dacl, 0, &mut ace_pointer) } == 0 || ace_pointer.is_null() {
            return Err(private_error("private object ACL is malformed"));
        }
        // SAFETY: every valid ACE starts with an ACE_HEADER.
        let header = unsafe { &*(ace_pointer.cast::<ACE_HEADER>()) };
        if header.AceType != 0 || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
            return Err(private_error("private object ACL is not owner-only"));
        }
        // SAFETY: the allowed-ACE type and size checks prove the fixed prefix is present.
        let ace = unsafe { &*(ace_pointer.cast::<ACCESS_ALLOWED_ACE>()) };
        if u32::from(header.AceFlags) & INHERITED_ACE != 0
            || ace.Mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS
        {
            return Err(private_error("private object ACL is not owner-only"));
        }
        let inheritance = u32::from(header.AceFlags) & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE);
        if (directory && inheritance != (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE))
            || (!directory && inheritance != 0)
        {
            return Err(private_error("private object ACL has unsafe inheritance"));
        }
        let ace_sid = (&ace.SidStart as *const u32).cast_mut().cast();
        if unsafe { EqualSid(ace_sid, current_sid) } == 0 {
            return Err(private_error("private object ACL grants another principal"));
        }
        Ok(())
    })
}

fn verify_private_owner(file: &File, directory: bool) -> Result<(), SecureFileError> {
    let identity = file_identity(file)?;
    if identity.is_reparse_point()
        || identity.is_directory() != directory
        || (!directory && identity.links != 1)
    {
        return Err(private_error("private object identity is unsafe"));
    }
    with_current_user_sid(|current_sid| {
        let mut owner: PSID = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: output pointers are writable and file owns a READ_CONTROL handle.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(win32_error(status).into());
        }
        let _descriptor = LocalAllocation(descriptor.cast());
        if owner.is_null() || unsafe { EqualSid(owner, current_sid) } == 0 {
            return Err(private_error(
                "private object is not owned by the current user",
            ));
        }
        Ok(())
    })
}

fn apply_private_acl(file: &File, directory: bool) -> Result<(), SecureFileError> {
    verify_private_owner(file, directory)?;
    with_current_user_sid(|sid| {
        let descriptor = build_private_descriptor(sid, directory)?;
        // SAFETY: file owns WRITE_DAC and the ACL allocation outlives this call.
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                descriptor.descriptor.Dacl,
                null(),
            )
        };
        if status != 0 {
            return Err(win32_error(status).into());
        }
        validate_private_acl(file, directory)
    })
}

fn make_private_acl(file: &File, directory: bool) -> Result<(), SecureFileError> {
    match validate_private_acl(file, directory) {
        Ok(()) => Ok(()),
        Err(SecureFileError::InsecurePrivateObject(_)) => apply_private_acl(file, directory),
        Err(error) => Err(error),
    }
}

fn read_open_file_bounded(mut file: &File, limit: usize) -> Result<Vec<u8>, SecureFileError> {
    let metadata = file.metadata()?;
    let advertised = usize::try_from(metadata.len()).map_err(|_| SecureFileError::TooLarge {
        limit,
        actual: u64::MAX,
    })?;
    if advertised > limit {
        return Err(SecureFileError::TooLarge {
            limit,
            actual: advertised as u64,
        });
    }
    let capacity = advertised.min(limit);
    let mut bytes = Vec::with_capacity(capacity);
    let mut take = (&mut file).take(limit.saturating_add(1) as u64);
    take.read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(SecureFileError::TooLarge {
            limit,
            actual: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

fn open_file_at(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    private_creation: bool,
) -> Result<(File, bool), SecureFileError> {
    let open = |descriptor| {
        nt_open_at(
            parent.as_raw_handle(),
            name,
            access,
            disposition,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
            descriptor,
        )
    };
    let result = if private_creation {
        with_private_descriptor(false, open)
    } else {
        open(null())
    };
    match result {
        Ok((file, information)) => Ok((file, information == 2)),
        Err(error) => {
            // `FILE_NON_DIRECTORY_FILE` refuses directories with an
            // access-denied shaped error, hiding "is a directory" from
            // callers that map `NotRegular` to a friendly diagnostic
            // (read/write tools). Probe with a directory open so those
            // callers keep working; any other failure keeps its error.
            if open_directory_at(parent, name, false, false).is_ok() {
                return Err(SecureFileError::NotRegular);
            }
            Err(error)
        }
    }
}

pub(super) fn create_private_directory_all(path: &Path) -> Result<(), SecureFileError> {
    let mut created = CreatedDirectories {
        entries: Vec::new(),
    };
    let result = (|| {
        let (root_path, names) = split_absolute(path)?;
        if names.is_empty() {
            return Err(invalid_path(path));
        }
        let mut directory = open_root(&root_path)?;
        for (index, component) in names.iter().enumerate() {
            let final_component = index + 1 == names.len();
            let (next, was_created) =
                match open_directory_at(&directory, component, false, final_component) {
                    Ok(result) => result,
                    Err(SecureFileError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        open_directory_at(&directory, component, true, true)?
                    }
                    Err(error) => return Err(error),
                };
            if was_created {
                created.record(&next)?;
            }
            if was_created || final_component {
                make_private_acl(&next, true)?;
            }
            directory = next;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            created.disarm();
            Ok(())
        }
        Err(error) => {
            created.rollback();
            Err(error)
        }
    }
}

pub(super) fn create_unique_private_directory(
    parent: &Path,
    prefix: &str,
) -> Result<(OsString, PrivateDirectoryIdentity, File), SecureFileError> {
    let parent = open_private_directory_for_lock(parent)?;
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let name = OsString::from(format!("{prefix}{}", random_temp_suffix()?));
        let created_directory = with_private_descriptor(true, |descriptor| {
            nt_open_at(
                parent.as_raw_handle(),
                &name,
                PRIVATE_DIRECTORY_CREATE_ACCESS,
                FILE_CREATE,
                FILE_DIRECTORY_FILE,
                FILE_ATTRIBUTE_DIRECTORY,
                descriptor,
            )
        });
        let (directory, _) = match created_directory {
            Ok(created) => created,
            Err(SecureFileError::Io(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut created = CreatedDirectories {
            entries: Vec::new(),
        };
        created.record(&directory)?;
        make_private_acl(&directory, true)?;

        let expected = file_identity(&directory)?;
        let (actual, _) = open_directory_at(&parent, &name, false, true)?;
        let actual = file_identity(&actual)?;
        if !actual.is_directory()
            || actual.is_reparse_point()
            || (actual.volume, actual.index) != (expected.volume, expected.index)
        {
            return Err(SecureFileError::Changed);
        }

        let identity = PrivateDirectoryIdentity {
            volume: expected.volume,
            index: expected.index,
        };
        created.disarm();
        return Ok((name, identity, directory));
    }
    Err(SecureFileError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique private directory",
    )))
}

pub(super) fn open_private_directory_for_lock(path: &Path) -> Result<File, SecureFileError> {
    let (root_path, names) = split_absolute(path)?;
    if names.is_empty() {
        return Err(invalid_path(path));
    }
    let mut directory = open_root(&root_path)?;
    for (index, component) in names.iter().enumerate() {
        let final_component = index + 1 == names.len();
        let (next, _) = open_directory_at(&directory, component, false, final_component)?;
        if final_component {
            validate_private_acl(&next, true)?;
        }
        directory = next;
    }
    Ok(directory)
}

fn open_bound_parent(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<(File, OsString), SecureFileError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| SecureFileError::InvalidPath(path.display().to_string()))?
        .to_os_string();
    let parent = open_private_directory_for_lock(parent_path)?;
    validate_bound_directory(&parent, expected)?;
    Ok((parent, name))
}

pub(super) fn remove_regular_file_if_exists(path: &Path) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (file, _) = match open_file_at(&parent, &name, FILE_GENERIC_READ | DELETE, FILE_OPEN, false)
    {
        Ok(result) => result,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    ensure_regular(file_identity(&file)?)?;
    delete_handle(&file)?;
    Ok(true)
}

pub(super) fn remove_regular_file_if_exists_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let (file, _) = match open_file_at(&parent, &name, FILE_GENERIC_READ | DELETE, FILE_OPEN, false)
    {
        Ok(result) => result,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    validate_private_acl(&file, false)?;
    delete_handle(&file)?;
    Ok(true)
}

#[cfg(test)]
pub(super) fn remove_empty_private_directory_if_exists(
    path: &Path,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let directory = match nt_open_at(
        parent.as_raw_handle(),
        &name,
        PRIVATE_DIRECTORY_INSPECTION_ACCESS | DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        FILE_ATTRIBUTE_DIRECTORY,
        null(),
    ) {
        Ok((directory, _)) => directory,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    validate_private_acl(&directory, true)?;
    delete_handle(&directory)?;
    Ok(true)
}

pub(super) fn remove_empty_private_directory_if_exists_bound(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<bool, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let directory = match nt_open_at(
        parent.as_raw_handle(),
        &name,
        PRIVATE_DIRECTORY_INSPECTION_ACCESS | DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        FILE_ATTRIBUTE_DIRECTORY,
        null(),
    ) {
        Ok((directory, _)) => directory,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    validate_bound_directory(&directory, expected)?;
    delete_handle(&directory)?;
    Ok(true)
}

pub(super) fn read_regular_file_bounded(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    read_open_regular(open_regular_file_for_read(path)?, limit)
}

pub(super) fn read_regular_file_bounded_by(
    path: &Path,
    upper_limit: usize,
    byte_limit: &dyn Fn(&[u8]) -> usize,
) -> Result<Vec<u8>, SecureFileError> {
    read_open_regular_bounded_by(open_regular_file_for_read(path)?, upper_limit, byte_limit)
}

pub(super) fn read_private_file_bounded(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, SecureFileError> {
    let file = open_private_file_for_read(path)?;
    read_open_file_bounded(&file, limit)
}

pub(super) fn open_private_file_for_read(path: &Path) -> Result<File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (file, _) = open_file_at(&parent, &name, FILE_GENERIC_READ, FILE_OPEN, false)?;
    validate_private_acl(&file, false)?;
    Ok(file)
}

pub(super) fn open_regular_file_for_read(path: &Path) -> Result<File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (file, _) = open_file_at(&parent, &name, FILE_GENERIC_READ, FILE_OPEN, false)?;
    ensure_regular(file_identity(&file)?)?;
    Ok(file)
}

#[cfg(test)]
pub(super) fn open_regular_file_for_read_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let (file, _) = open_file_at(&parent, &name, FILE_GENERIC_READ, FILE_OPEN, false)?;
    validate_private_acl(&file, false)?;
    Ok(file)
}

pub(super) fn open_regular_file_for_append(path: &Path) -> Result<File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (mut file, _) = open_file_at(&parent, &name, APPEND_ACCESS, FILE_OPEN, false)?;
    ensure_regular(file_identity(&file)?)?;
    file.seek(std::io::SeekFrom::End(0))?;
    Ok(file)
}

pub(super) fn open_regular_file_for_append_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let (mut file, _) = open_file_at(&parent, &name, APPEND_ACCESS, FILE_OPEN, false)?;
    validate_private_acl(&file, false)?;
    file.seek(std::io::SeekFrom::End(0))?;
    Ok(file)
}

pub(super) fn create_regular_file_for_append(path: &Path) -> Result<File, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (file, _) = open_file_at(&parent, &name, APPEND_ACCESS, FILE_CREATE, true)?;
    ensure_regular(file_identity(&file)?)?;
    Ok(file)
}

pub(super) fn create_regular_file_for_append_in(
    path: &Path,
    expected: &PrivateDirectoryIdentity,
) -> Result<File, SecureFileError> {
    let (parent, name) = open_bound_parent(path, expected)?;
    let (file, _) = open_file_at(&parent, &name, APPEND_ACCESS, FILE_CREATE, true)?;
    validate_private_acl(&file, false)?;
    Ok(file)
}

pub(super) fn open_private_lock_file(path: &Path) -> Result<File, SecureFileError> {
    let (parent, name) = open_parent(path, true)?;
    for _ in 0..16 {
        match open_file_at(&parent, &name, PRIVATE_INSPECTION_ACCESS, FILE_OPEN, false) {
            Ok((existing, _)) => {
                verify_private_owner(&existing, false)?;
                return Ok(existing);
            }
            Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                match open_file_at(&parent, &name, PRIVATE_FILE_ACCESS, FILE_CREATE, true) {
                    Ok((file, _)) => {
                        validate_private_acl(&file, false)?;
                        return Ok(file);
                    }
                    Err(SecureFileError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(SecureFileError::Changed)
}

pub(super) struct PrivateLockIdentity(FileIdentity);

fn current_private_lock_identity(path: &Path) -> Result<FileIdentity, SecureFileError> {
    let (parent, name) = open_parent(path, false)?;
    let (file, _) = open_file_at(&parent, &name, PRIVATE_INSPECTION_ACCESS, FILE_OPEN, false)?;
    validate_private_acl(&file, false)?;
    file_identity(&file)
}

pub(super) fn validate_private_lock_after_acquire(
    path: &Path,
    file: &File,
) -> Result<PrivateLockIdentity, SecureFileError> {
    make_private_acl(file, false)?;
    let identity = file_identity(file)?;
    if current_private_lock_identity(path)? != identity {
        return Err(SecureFileError::Changed);
    }
    Ok(PrivateLockIdentity(identity))
}

pub(super) fn revalidate_private_lock_before_release(
    path: &Path,
    file: &File,
    expected: &PrivateLockIdentity,
) -> Result<(), SecureFileError> {
    validate_private_acl(file, false)?;
    if file_identity(file)? != expected.0 || current_private_lock_identity(path)? != expected.0 {
        return Err(SecureFileError::Changed);
    }
    Ok(())
}

enum Original {
    Missing,
    Regular {
        bytes: Vec<u8>,
        identity: FileIdentity,
    },
}

impl Original {
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Regular { bytes, .. } => Some(bytes),
            Self::Missing => None,
        }
    }
}

fn inspect_target(
    parent: &File,
    name: &OsStr,
    limit: usize,
    private: bool,
    repair_private: bool,
) -> Result<(Original, Option<File>), SecureFileError> {
    let access = if private {
        PRIVATE_INSPECTION_ACCESS
    } else {
        FILE_GENERIC_READ
    };
    let (file, _) = match open_file_at(parent, name, access, FILE_OPEN, false) {
        Ok(result) => result,
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Original::Missing, None));
        }
        Err(error) => return Err(error),
    };
    let identity = file_identity(&file)?;
    ensure_regular(identity)?;
    if private {
        if repair_private {
            make_private_acl(&file, false)?;
        } else {
            validate_private_acl(&file, false)?;
        }
    }
    let bytes = read_open_file_bounded(&file, limit)?;
    if file_identity(&file)? != identity {
        return Err(SecureFileError::Changed);
    }
    Ok((Original::Regular { bytes, identity }, Some(file)))
}

fn unchanged(
    parent: &File,
    name: &OsStr,
    original: &Original,
    limit: usize,
    private: bool,
) -> Result<Option<File>, SecureFileError> {
    let (current, handle) = inspect_target(parent, name, limit, private, false)?;
    let same = match (original, current) {
        (Original::Missing, Original::Missing) => true,
        (
            Original::Regular { bytes, identity },
            Original::Regular {
                bytes: current_bytes,
                identity: current_identity,
            },
        ) => *identity == current_identity && *bytes == current_bytes,
        _ => false,
    };
    if !same {
        return Err(SecureFileError::Changed);
    }
    Ok(handle)
}

/// Rename `file` to `name` inside the directory bound by `parent`.
///
/// This uses the native call rather than the Win32
/// `SetFileInformationByHandle(FileRenameInfo)` wrapper, which documents
/// `FileName` as a NUL-terminated path that may be resolved against the
/// current directory. `NtSetInformationFile` resolves the length-counted
/// name strictly relative to `RootDirectory`, so publication stays bound to
/// the parent handle opened by the path walk.
fn rename_handle(
    file: &File,
    parent: &File,
    name: &OsStr,
    replace: bool,
) -> Result<(), SecureFileError> {
    let invalid = || SecureFileError::InvalidPath(name.to_string_lossy().into_owned());
    let wide = name.encode_wide().collect::<Vec<_>>();
    let name_bytes = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(invalid)?;
    let offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    // Keep a trailing NUL, and never pass less than the fixed structure
    // size that the I/O manager validates for a short name.
    let bytes = offset
        .checked_add(name_bytes as usize)
        .and_then(|length| length.checked_add(size_of::<u16>()))
        .map(|length| length.max(size_of::<FILE_RENAME_INFORMATION>()))
        .ok_or_else(invalid)?;
    let length = u32::try_from(bytes).map_err(|_| invalid())?;
    let words = bytes.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the zeroed, aligned buffer is at least offset + name_bytes + 2 bytes long.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            wide.as_ptr().cast::<u8>(),
            buffer.as_mut_ptr().cast::<u8>().add(offset),
            name_bytes as usize,
        );
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: info points to a correctly sized FILE_RENAME_INFORMATION
    // buffer, and both handles stay open for this synchronous call.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            info.cast(),
            length,
            FileRenameInformation,
        )
    };
    if status < 0 {
        let error = ntstatus_error(status);
        if !replace && error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(SecureFileError::Changed);
        }
        return Err(error.into());
    }
    Ok(())
}

fn delete_handle(file: &File) -> Result<(), SecureFileError> {
    let extended = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the input struct and handle remain valid for this call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&extended as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0
    {
        return Ok(());
    }
    let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: fallback for Windows versions lacking FileDispositionInfoEx.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&legacy as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(last_error().into());
    }
    Ok(())
}

/// Open the existing target so that no other opener can write, rename,
/// delete, or replace it, then verify through that handle that it is still
/// exactly the object and bytes observed during preparation.
///
/// Windows has no atomic exchange, so this pin is what binds the
/// replacement to the verified object: from here until the pinned handle is
/// renamed away, the target name cannot come to refer to anything else.
fn pin_unchanged(
    parent: &File,
    name: &OsStr,
    original: &Original,
    limit: usize,
    private: bool,
) -> Result<File, SecureFileError> {
    let Original::Regular { bytes, identity } = original else {
        return Err(SecureFileError::Changed);
    };
    let mut attempts = 0;
    let pinned = loop {
        attempts += 1;
        match nt_open_at_shared(
            parent.as_raw_handle(),
            name,
            PIN_ACCESS,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
            null(),
            PIN_SHARE,
        ) {
            Ok((file, _)) => break file,
            Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SecureFileError::Changed);
            }
            Err(SecureFileError::Io(error))
                if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    && attempts < PIN_SHARING_ATTEMPTS =>
            {
                std::thread::sleep(PIN_SHARING_BACKOFF);
            }
            Err(error) => return Err(error),
        }
    };
    let current = file_identity(&pinned)?;
    ensure_regular(current)?;
    if private {
        validate_private_acl(&pinned, false)?;
    }
    if current != *identity
        || read_open_file_bounded(&pinned, limit)? != *bytes
        || file_identity(&pinned)? != *identity
    {
        return Err(SecureFileError::Changed);
    }
    Ok(pinned)
}

/// The name a replacement is published under: the pinned file's own
/// spelling when that name, looked up in the same bound parent, is the
/// pinned object. NTFS matches names case-insensitively (and by 8.3
/// alias), so republishing under the caller's spelling would otherwise
/// rename `README.md` to `readme.md`.
fn published_name(parent: &File, pinned: &File, requested: &OsStr) -> OsString {
    let on_disk = (|| {
        // FileNameInfo reports the volume-relative path of this open;
        // UNICODE_STRING bounds it to 32767 UTF-16 units.
        let bytes = size_of::<FILE_NAME_INFO>() + 2 * usize::from(u16::MAX);
        let mut buffer = vec![0_u32; bytes.div_ceil(size_of::<u32>())];
        // SAFETY: the aligned buffer is writable for `bytes` bytes and the handle is open.
        if unsafe {
            GetFileInformationByHandleEx(
                pinned.as_raw_handle(),
                FileNameInfo,
                buffer.as_mut_ptr().cast(),
                bytes as u32,
            )
        } == 0
        {
            return None;
        }
        let info = buffer.as_ptr().cast::<FILE_NAME_INFO>();
        // SAFETY: a successful call initializes FileNameLength bytes of
        // FileName, which lie inside the buffer.
        let path = unsafe {
            std::slice::from_raw_parts(
                (*info).FileName.as_ptr(),
                (*info).FileNameLength as usize / size_of::<u16>(),
            )
        };
        let last = path.rsplit(|unit| *unit == u16::from(b'\\')).next()?;
        Some(OsString::from_wide(last))
    })();
    let Some(on_disk) = on_disk else {
        return requested.to_os_string();
    };
    if on_disk.as_os_str() == requested || !component_is_safe(&on_disk) {
        return requested.to_os_string();
    }
    let names_pinned = open_file_at(
        parent,
        &on_disk,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        false,
    )
    .and_then(|(alias, _)| Ok((file_identity(&alias)?, file_identity(pinned)?)))
    .is_ok_and(|(alias, pinned)| (alias.volume, alias.index) == (pinned.volume, pinned.index));
    if names_pinned {
        on_disk
    } else {
        requested.to_os_string()
    }
}

/// Carry the replaced file's user-visible attributes (read-only, hidden,
/// system, not-indexed) over to its replacement; the replacement is new
/// content, so it is also marked for archiving.
fn carry_attributes(file: &File, original: FileIdentity) -> Result<(), SecureFileError> {
    let basic = FILE_BASIC_INFO {
        // Zero timestamps leave the replacement's own times unchanged.
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: (original.attributes & PRESERVED_ATTRIBUTES) | FILE_ATTRIBUTE_ARCHIVE,
    };
    // SAFETY: the input struct and handle remain valid for this call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            (&basic as *const FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(last_error().into());
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Runs once between displacing a pinned target and publishing its
    /// replacement, so tests can race that window deterministically.
    pub(super) static AFTER_DISPLACEMENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Replace the pinned target with `staged`: rename the pinned object to a
/// private backup name, publish `staged` under the target name with a
/// no-replace rename, then delete the backup by handle.
///
/// Every step is bound to the parent handle and to the two file handles,
/// and no step can overwrite a name it did not vacate itself. Between the
/// two renames, readers can briefly find the target missing. If another
/// writer creates the target in that window it wins: the staged file is
/// not published and the displaced original stays under its backup name
/// rather than being destroyed.
fn replace_pinned(
    parent: &File,
    name: &OsStr,
    staged: &File,
    pinned: File,
) -> Result<(), SecureFileError> {
    let published = published_name(parent, &pinned, name);
    let mut displaced = false;
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let backup = OsString::from(format!(".octet-old-{}", random_temp_suffix()?));
        match rename_handle(&pinned, parent, &backup, false) {
            Ok(()) => {
                displaced = true;
                break;
            }
            // A no-replace rename reports an occupied name as Changed.
            Err(SecureFileError::Changed) => continue,
            Err(error) => return Err(error),
        }
    }
    if !displaced {
        return Err(SecureFileError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique secure backup name",
        )));
    }
    #[cfg(test)]
    if let Some(hook) = AFTER_DISPLACEMENT.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    match rename_handle(staged, parent, &published, false) {
        Ok(()) => {
            // Best effort, as on Unix: the replacement is already
            // published, and a leftover backup holds only the old bytes.
            let _ = delete_handle(&pinned);
            Ok(())
        }
        Err(error) => {
            // Restore only into a still-free name.
            let _ = rename_handle(&pinned, parent, &published, false);
            Err(error)
        }
    }
}

pub(super) struct PreparedMutation {
    parent: File,
    name: OsString,
    original: Original,
    limit: usize,
    private: bool,
}

impl PreparedMutation {
    pub(super) fn prepare(
        path: &Path,
        create_parents: bool,
        limit: usize,
    ) -> Result<Self, SecureFileError> {
        Self::prepare_inner(path, create_parents, limit, false)
    }

    pub(super) fn prepare_private(path: &Path, limit: usize) -> Result<Self, SecureFileError> {
        Self::prepare_inner(path, false, limit, true)
    }

    fn prepare_inner(
        path: &Path,
        create_parents: bool,
        limit: usize,
        private: bool,
    ) -> Result<Self, SecureFileError> {
        let (parent, name) = open_parent(path, create_parents)?;
        let (original, _handle) = inspect_target(&parent, &name, limit, private, private)?;
        Ok(Self {
            parent,
            name,
            original,
            limit,
            private,
        })
    }

    pub(super) fn original(&self) -> Option<&[u8]> {
        self.original.bytes()
    }

    /// Delete the target only while it is still the prepared object: the
    /// pinned handle is verified and then deleted by handle, so a
    /// concurrent replacement is never removed.
    pub(super) fn remove(self) -> Result<(), SecureFileError> {
        let pinned = pin_unchanged(
            &self.parent,
            &self.name,
            &self.original,
            self.limit,
            self.private,
        )?;
        delete_handle(&pinned)
    }

    pub(super) fn commit(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        self.commit_inner(data, cancelled, false)
    }

    pub(super) fn commit_private(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), SecureFileError> {
        self.commit_inner(data, cancelled, true)
    }

    fn commit_inner(
        self,
        data: &[u8],
        cancelled: &dyn Fn() -> bool,
        private: bool,
    ) -> Result<(), SecureFileError> {
        if private != self.private {
            return Err(private_error("private mutation mode changed"));
        }
        // Fail early, before staging, when the target already changed.
        drop(unchanged(
            &self.parent,
            &self.name,
            &self.original,
            self.limit,
            private,
        )?);
        let mut temp = {
            let mut created = None;
            for _ in 0..TEMP_NAME_ATTEMPTS {
                let candidate = OsString::from(format!(".octet-tmp-{}", random_temp_suffix()?));
                match open_file_at(
                    &self.parent,
                    &candidate,
                    PRIVATE_FILE_ACCESS,
                    FILE_CREATE,
                    private,
                ) {
                    Ok((file, _)) => {
                        created = Some(file);
                        break;
                    }
                    Err(SecureFileError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            created.ok_or_else(|| {
                SecureFileError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "could not allocate a unique secure temporary file",
                ))
            })?
        };
        let result = (|| -> Result<(), SecureFileError> {
            ensure_regular(file_identity(&temp)?)?;
            if private {
                make_private_acl(&temp, false)?;
            }
            for chunk in data.chunks(64 * 1024) {
                if cancelled() {
                    return Err(SecureFileError::Cancelled);
                }
                temp.write_all(chunk)?;
            }
            if cancelled() {
                return Err(SecureFileError::Cancelled);
            }
            if !private {
                if let Original::Regular { identity, .. } = &self.original {
                    carry_attributes(&temp, *identity)?;
                }
            }
            temp.sync_all()?;
            match &self.original {
                Original::Missing => {
                    drop(unchanged(
                        &self.parent,
                        &self.name,
                        &self.original,
                        self.limit,
                        private,
                    )?);
                    if cancelled() {
                        return Err(SecureFileError::Cancelled);
                    }
                    // A no-replace rename never overwrites a target
                    // created after the check above.
                    rename_handle(&temp, &self.parent, &self.name, false)
                }
                Original::Regular { .. } => {
                    // Never fall back to ReplaceIfExists: that would
                    // overwrite whatever the name refers to at that
                    // moment. The pin keeps it the verified object.
                    let pinned = pin_unchanged(
                        &self.parent,
                        &self.name,
                        &self.original,
                        self.limit,
                        private,
                    )?;
                    if cancelled() {
                        return Err(SecureFileError::Cancelled);
                    }
                    replace_pinned(&self.parent, &self.name, &temp, pinned)
                }
            }
        })();
        if result.is_err() {
            let _ = delete_handle(&temp);
        }
        result
    }
}
