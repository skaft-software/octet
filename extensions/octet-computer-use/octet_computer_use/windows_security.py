"""Windows equivalents of the extension's POSIX filesystem protections.

POSIX code in this bundle relies on ``O_NOFOLLOW``, ``lstat`` link checks, and
0600/0700 modes. Windows has none of those semantics:

* ``lstat`` reports a directory junction as a directory, so a link check must
  also reject reparse points;
* ``O_NOFOLLOW`` does not exist, so a staged file is verified after opening by
  its final path, which exposes a junction or symlink swapped into any parent;
* permission bits map only to the read-only attribute, so a private file gets a
  protected DACL with one full-access entry for the current user, matching the
  host's private objects.

Everything here is importable on every platform. The Win32 calls run only on
Windows and return a failure value, never a best-effort success, when an API is
unavailable.
"""

from __future__ import annotations

import os
import stat
import sys
from typing import Optional

IS_WINDOWS = sys.platform == "win32"

_FILE_ATTRIBUTE_REPARSE_POINT = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
_DACL_SECURITY_INFORMATION = 0x00000004
_PROTECTED_DACL_SECURITY_INFORMATION = 0x80000000
_SDDL_REVISION_1 = 1
_TOKEN_QUERY = 0x0008
_TOKEN_USER = 1
_MAX_SECURITY_BYTES = 64 * 1024


def is_link_or_reparse_point(metadata: os.stat_result) -> bool:
    """Whether ``lstat``/``fstat`` metadata names a link of any kind."""

    if stat.S_ISLNK(metadata.st_mode):
        return True
    attributes = getattr(metadata, "st_file_attributes", 0) or 0
    return bool(attributes & _FILE_ATTRIBUTE_REPARSE_POINT)


def _strip_verbatim_prefix(path: str) -> str:
    if path.startswith("\\\\?\\UNC\\"):
        return "\\\\" + path[len("\\\\?\\UNC\\"):]
    if path.startswith("\\\\?\\"):
        return path[len("\\\\?\\"):]
    return path


def final_path(fd: int) -> Optional[str]:
    """The normalized path of an open file descriptor, with links resolved."""

    if not IS_WINDOWS:
        return None
    try:
        import ctypes
        import msvcrt
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        get_final_path = kernel32.GetFinalPathNameByHandleW
        get_final_path.argtypes = [wintypes.HANDLE, wintypes.LPWSTR, wintypes.DWORD, wintypes.DWORD]
        get_final_path.restype = wintypes.DWORD
        handle = msvcrt.get_osfhandle(fd)
        size = 512
        for _ in range(4):
            buffer = ctypes.create_unicode_buffer(size)
            length = get_final_path(handle, buffer, size, 0)
            if length == 0:
                return None
            if length < size:
                return _strip_verbatim_prefix(buffer.value)
            size = length + 1
    except (OSError, AttributeError, ValueError):
        return None
    return None


def opened_at(fd: int, expected: str) -> bool:
    """Whether ``fd`` is the file at ``expected`` with no redirection."""

    actual = final_path(fd)
    if actual is None:
        return False
    return os.path.normcase(os.path.normpath(actual)) == os.path.normcase(os.path.normpath(expected))


def _advapi32():
    import ctypes
    from ctypes import wintypes

    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi32.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi32.OpenProcessToken.restype = wintypes.BOOL
    advapi32.GetTokenInformation.argtypes = [
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
    ]
    advapi32.GetTokenInformation.restype = wintypes.BOOL
    advapi32.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    advapi32.ConvertSidToStringSidW.restype = wintypes.BOOL
    advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.ULONG),
    ]
    advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW.restype = wintypes.BOOL
    advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW.argtypes = [
        ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(wintypes.LPWSTR), ctypes.POINTER(wintypes.ULONG),
    ]
    advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW.restype = wintypes.BOOL
    advapi32.SetFileSecurityW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_void_p]
    advapi32.SetFileSecurityW.restype = wintypes.BOOL
    advapi32.GetFileSecurityW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
    ]
    advapi32.GetFileSecurityW.restype = wintypes.BOOL
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    return ctypes, wintypes, advapi32, kernel32


def current_user_sid() -> Optional[str]:
    """The current process user's SID in string form."""

    if not IS_WINDOWS:
        return None
    try:
        ctypes, wintypes, advapi32, kernel32 = _advapi32()
        token = wintypes.HANDLE()
        if not advapi32.OpenProcessToken(kernel32.GetCurrentProcess(), _TOKEN_QUERY, ctypes.byref(token)):
            return None
        try:
            needed = wintypes.DWORD()
            advapi32.GetTokenInformation(token, _TOKEN_USER, None, 0, ctypes.byref(needed))
            if not 0 < needed.value <= _MAX_SECURITY_BYTES:
                return None
            buffer = ctypes.create_string_buffer(needed.value)
            if not advapi32.GetTokenInformation(token, _TOKEN_USER, buffer, needed, ctypes.byref(needed)):
                return None
            # TOKEN_USER begins with SID_AND_ATTRIBUTES, whose first field is the SID pointer.
            sid = ctypes.cast(buffer, ctypes.POINTER(ctypes.c_void_p))[0]
            text = wintypes.LPWSTR()
            if not advapi32.ConvertSidToStringSidW(sid, ctypes.byref(text)):
                return None
            try:
                return text.value
            finally:
                kernel32.LocalFree(text)
        finally:
            kernel32.CloseHandle(token)
    except (OSError, AttributeError, ValueError):
        return None


def private_dacl(sid: str, *, directory: bool = False) -> str:
    """SDDL for a protected DACL granting only ``sid`` full access."""

    inheritance = "OICI" if directory else ""
    return f"D:P(A;{inheritance};FA;;;{sid})"


def restrict_to_current_user(path: os.PathLike | str, *, directory: bool = False) -> bool:
    """Replace ``path``'s DACL with a protected current-user-only DACL.

    Returns False, never a partial success, when the user or ACL cannot be
    established; callers holding secrets must then fail closed.
    """

    if not IS_WINDOWS:
        return False
    sid = current_user_sid()
    if not sid:
        return False
    try:
        ctypes, wintypes, advapi32, kernel32 = _advapi32()
        descriptor = ctypes.c_void_p()
        if not advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            private_dacl(sid, directory=directory), _SDDL_REVISION_1, ctypes.byref(descriptor), None,
        ):
            return False
        try:
            return bool(advapi32.SetFileSecurityW(
                os.fspath(path),
                _DACL_SECURITY_INFORMATION | _PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            ))
        finally:
            kernel32.LocalFree(descriptor)
    except (OSError, AttributeError, ValueError):
        return False


def describe_dacl(path: os.PathLike | str) -> Optional[str]:
    """The DACL of ``path`` in SDDL form, for verification."""

    if not IS_WINDOWS:
        return None
    try:
        ctypes, wintypes, advapi32, kernel32 = _advapi32()
        needed = wintypes.DWORD()
        advapi32.GetFileSecurityW(os.fspath(path), _DACL_SECURITY_INFORMATION, None, 0, ctypes.byref(needed))
        if not 0 < needed.value <= _MAX_SECURITY_BYTES:
            return None
        buffer = ctypes.create_string_buffer(needed.value)
        if not advapi32.GetFileSecurityW(
            os.fspath(path), _DACL_SECURITY_INFORMATION, buffer, needed, ctypes.byref(needed),
        ):
            return None
        text = wintypes.LPWSTR()
        if not advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW(
            buffer, _SDDL_REVISION_1, _DACL_SECURITY_INFORMATION, ctypes.byref(text), None,
        ):
            return None
        try:
            return text.value
        finally:
            kernel32.LocalFree(text)
    except (OSError, AttributeError, ValueError):
        return None


def is_private_to_current_user(path: os.PathLike | str) -> bool:
    """Whether ``path`` has a protected DACL whose only entry grants the
    current user full access."""

    sid = current_user_sid()
    descriptor = describe_dacl(path)
    if not sid or not descriptor or not descriptor.startswith("D:"):
        return False
    flags, _, aces = descriptor[len("D:"):].partition("(")
    if "P" not in flags or not aces.endswith(")"):
        return False
    entries = aces[:-1].split(")(")
    if len(entries) != 1:
        return False
    # ace_type;ace_flags;rights;object_guid;inherit_object_guid;account_sid
    fields = entries[0].split(";")
    return len(fields) == 6 and fields[0] == "A" and fields[2] == "FA" and fields[5] == sid


__all__ = [
    "IS_WINDOWS",
    "current_user_sid",
    "describe_dacl",
    "final_path",
    "is_link_or_reparse_point",
    "is_private_to_current_user",
    "opened_at",
    "private_dacl",
    "restrict_to_current_user",
]
