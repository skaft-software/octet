//! C ABI v1 for this same executable runtime. See `sdk/c/include/octet.h`.
//!
//! # Safety
//! All non-null pointers must be valid, aligned live allocations of the specified
//! type/length. Handles come only from this library; no forged, stale or duplicate
//! handles. Extension functions are single-threaded and forbidden while run is
//! active. Calls/accessors are callback-thread-only. Callback/user data stay alive
//! through run; borrowed argument text stays alive through that callback only.
//! Nulls and excessive lengths are checked, not arbitrary memory accessibility.
use crate::{CallContext, Error, Extension, ToolResult, MAX_TEXT_BYTES};
use serde_json::{json, Map, Value};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::time::Duration;

/// UTF-8 bytes, never NUL-terminated. Null is allowed only for length zero.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OctetStr {
    pub data: *const u8,
    pub len: usize,
}
impl Default for OctetStr {
    fn default() -> Self {
        Self {
            data: ptr::null(),
            len: 0,
        }
    }
}
/// A flat typed field. `kind`: 1 string, 2 integer, 3 boolean. `required`: 0/1.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OctetField {
    pub name: OctetStr,
    pub kind: u32,
    pub required: u32,
    pub max_length: usize,
    pub minimum: i64,
    pub maximum: i64,
}
/// Owned extension handle, opaque in C.
pub struct OctetExtension {
    extension: Option<Extension>,
}
/// Borrowed callback handle, opaque in C.
pub struct OctetCall {
    arguments: Value,
    context: CallContext,
    result: Option<ToolResult>,
}
/// C callback returns a status code; it must not throw or unwind.
pub type OctetCallback = unsafe extern "C" fn(*mut OctetCall, *mut c_void) -> i32;

/// ABI status codes (not JSON-RPC codes).
pub const OK: i32 = 0;
pub const INVALID: i32 = 1;
pub const MISSING: i32 = 2;
pub const TYPE: i32 = 3;
pub const BOUNDS: i32 = 4;
pub const STATE: i32 = 5;
pub const PANIC: i32 = 6;
pub const CANCELLED: i32 = 7;
pub const RUNTIME: i32 = 8;

fn guard(f: impl FnOnce() -> Result<(), i32>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}
unsafe fn aligned<'a, T>(pointer: *const T) -> Result<&'a T, i32> {
    if pointer.is_null() || !(pointer as usize).is_multiple_of(std::mem::align_of::<T>()) {
        return Err(INVALID);
    }
    Ok(&*pointer)
}
unsafe fn aligned_mut<'a, T>(pointer: *mut T) -> Result<&'a mut T, i32> {
    if pointer.is_null() || !(pointer as usize).is_multiple_of(std::mem::align_of::<T>()) {
        return Err(INVALID);
    }
    Ok(&mut *pointer)
}
unsafe fn text<'a>(value: OctetStr, cap: usize) -> Result<&'a str, i32> {
    if value.len > cap {
        return Err(BOUNDS);
    }
    if value.len == 0 {
        return Ok("");
    }
    if value.data.is_null() {
        return Err(INVALID);
    }
    std::str::from_utf8(std::slice::from_raw_parts(value.data, value.len)).map_err(|_| TYPE)
}
unsafe fn argument<'a>(call: *const OctetCall, name: OctetStr) -> Result<&'a Value, i32> {
    let call = aligned(call)?;
    let name = text(name, 256)?;
    call.arguments.get(name).ok_or(MISSING)
}

/// Return the native C ABI version, independent of SDK/API distribution versions.
#[no_mangle]
pub extern "C" fn octet_abi_version() -> u32 {
    1
}
/// Allocate an empty extension. `out` must point to an empty handle slot.
/// # Safety
/// Follow the module's pointer/ownership contract.
#[no_mangle]
pub unsafe extern "C" fn octet_extension_new(out: *mut *mut OctetExtension) -> i32 {
    guard(|| {
        let out = aligned_mut(out)?;
        if !out.is_null() {
            return Err(STATE);
        }
        *out = Box::into_raw(Box::new(OctetExtension {
            extension: Some(Extension::new()),
        }));
        Ok(())
    })
}
/// Destroy and null an owned handle. Null handle slots are idempotent.
/// # Safety
/// No aliases, active run or retained callbacks may use this extension.
#[no_mangle]
pub unsafe extern "C" fn octet_extension_free(handle: *mut *mut OctetExtension) -> i32 {
    guard(|| {
        let slot = aligned_mut(handle)?;
        if slot.is_null() {
            return Ok(());
        }
        aligned(*slot)?;
        let old = *slot;
        *slot = ptr::null_mut();
        drop(Box::from_raw(old));
        Ok(())
    })
}
/// Register a flat typed tool; strings/fields are copied before returning.
/// # Safety
/// Pointers follow the module contract. Callback/user data live through run.
#[no_mangle]
pub unsafe extern "C" fn octet_tool_add(
    handle: *mut OctetExtension,
    name: OctetStr,
    description: OctetStr,
    fields: *const OctetField,
    field_count: usize,
    callback: Option<OctetCallback>,
    user_data: *mut c_void,
) -> i32 {
    guard(|| {
        let extension = aligned_mut(handle)?.extension.as_mut().ok_or(STATE)?;
        let name = text(name, 64)?;
        let description = text(description, 4096)?;
        if field_count > 64 {
            return Err(BOUNDS);
        }
        let fields = if field_count == 0 {
            &[]
        } else {
            aligned(fields)?;
            std::slice::from_raw_parts(fields, field_count)
        };
        let callback = callback.ok_or(INVALID)?;
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in fields {
            let key = text(field.name, 256)?;
            if key.is_empty() || properties.contains_key(key) || field.required > 1 {
                return Err(INVALID);
            }
            let schema = match field.kind {
                1 => {
                    if field.max_length > MAX_TEXT_BYTES {
                        return Err(BOUNDS);
                    }
                    if field.minimum != 0 || field.maximum != 0 {
                        return Err(INVALID);
                    }
                    json!({"type":"string","maxLength":field.max_length})
                }
                2 => {
                    if field.max_length != 0 || field.minimum > field.maximum {
                        return Err(INVALID);
                    }
                    if field.minimum < -9_007_199_254_740_991
                        || field.maximum > 9_007_199_254_740_991
                    {
                        return Err(BOUNDS);
                    }
                    json!({"type":"integer","minimum":field.minimum,"maximum":field.maximum})
                }
                3 => {
                    if field.max_length != 0 || field.minimum != 0 || field.maximum != 0 {
                        return Err(INVALID);
                    }
                    json!({"type":"boolean"})
                }
                _ => return Err(TYPE),
            };
            properties.insert(key.into(), schema);
            if field.required == 1 {
                required.push(key.to_owned());
            }
        }
        // Foreign memory isn't made safe by a Rust marker trait. The author
        // explicitly promises a valid callback and user pointer on the worker.
        let user = user_data as usize;
        extension.add_tool(name, description, json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}), move |arguments, context| {
            let mut call = OctetCall { arguments, context, result: None };
            let status = callback(&mut call, user as *mut c_void);
            match status {
                OK => call.result.ok_or_else(|| Error::rpc(-32603, "C callback omitted its result")),
                CANCELLED => Err(Error::cancelled()),
                _ => Err(Error::tool(format!("native callback failed (status {status})"))),
            }
        }).map_err(|_| INVALID)?;
        Ok(())
    })
}
/// Run the same runtime once. Tool registrations are consumed; handle can be freed.
/// # Safety
/// Handle/callbacks/user data follow the module contract; see `Extension::run`.
#[no_mangle]
pub unsafe extern "C" fn octet_extension_run(handle: *mut OctetExtension) -> i32 {
    guard(|| {
        let extension = aligned_mut(handle)?.extension.take().ok_or(STATE)?;
        extension.run().map_err(|_| RUNTIME)
    })
}
/// Borrow a string argument until callback return. Output is zeroed on failure.
/// # Safety
/// Live callback handle, valid name and aligned output slot are required.
#[no_mangle]
pub unsafe extern "C" fn octet_arg_string(
    call: *const OctetCall,
    name: OctetStr,
    out: *mut OctetStr,
) -> i32 {
    guard(|| {
        let out = aligned_mut(out)?;
        *out = OctetStr::default();
        let value = argument(call, name)?.as_str().ok_or(TYPE)?;
        *out = OctetStr {
            data: value.as_ptr(),
            len: value.len(),
        };
        Ok(())
    })
}
/// Copy a signed integer argument. Output is zeroed on failure.
/// # Safety
/// Live callback handle, valid name and aligned output slot are required.
#[no_mangle]
pub unsafe extern "C" fn octet_arg_integer(
    call: *const OctetCall,
    name: OctetStr,
    out: *mut i64,
) -> i32 {
    guard(|| {
        let out = aligned_mut(out)?;
        *out = 0;
        *out = argument(call, name)?.as_i64().ok_or(TYPE)?;
        Ok(())
    })
}
/// Copy a boolean argument (0/1). Output is zeroed on failure.
/// # Safety
/// Live callback handle, valid name and aligned output slot are required.
#[no_mangle]
pub unsafe extern "C" fn octet_arg_boolean(
    call: *const OctetCall,
    name: OctetStr,
    out: *mut u32,
) -> i32 {
    guard(|| {
        let out = aligned_mut(out)?;
        *out = 0;
        *out = u32::from(argument(call, name)?.as_bool().ok_or(TYPE)?);
        Ok(())
    })
}
/// Return OK or CANCELLED; null handles return INVALID.
/// # Safety
/// A live callback handle is required.
#[no_mangle]
pub unsafe extern "C" fn octet_check_cancelled(call: *const OctetCall) -> i32 {
    guard(|| {
        if aligned(call)?.context.is_cancelled() {
            Err(CANCELLED)
        } else {
            Ok(())
        }
    })
}
/// Interruptible wait; milliseconds are bounded to 0..60000.
/// # Safety
/// A live callback handle is required.
#[no_mangle]
pub unsafe extern "C" fn octet_wait(call: *const OctetCall, milliseconds: u32) -> i32 {
    guard(|| {
        let call = aligned(call)?;
        if milliseconds > 60_000 {
            return Err(BOUNDS);
        }
        call.context
            .wait(Duration::from_millis(milliseconds.into()))
            .map_err(|_| CANCELLED)
    })
}
/// Copy exactly one UTF-8 result. `is_error` must be 0/1. Text cap is 128 KiB.
/// # Safety
/// A live callback handle and valid text bytes are required. No retention occurs.
#[no_mangle]
pub unsafe extern "C" fn octet_result_text(
    call: *mut OctetCall,
    value: OctetStr,
    is_error: u32,
) -> i32 {
    guard(|| {
        let call = aligned_mut(call)?;
        if call.result.is_some() {
            return Err(STATE);
        }
        if is_error > 1 {
            return Err(INVALID);
        }
        let text = text(value, MAX_TEXT_BYTES)?.to_owned();
        call.result = Some(if is_error == 1 {
            ToolResult::error(text)
        } else {
            ToolResult::text(text)
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panics_do_not_escape_guard() {
        assert_eq!(guard(|| panic!("test panic")), PANIC);
    }
    #[test]
    fn null_length_utf8_and_lifetime_contracts() {
        unsafe {
            assert_eq!(octet_extension_new(ptr::null_mut()), INVALID);
            assert_eq!(octet_check_cancelled(ptr::null()), INVALID);
            assert_eq!(
                text(
                    OctetStr {
                        data: ptr::null(),
                        len: usize::MAX
                    },
                    256
                ),
                Err(BOUNDS)
            );
            assert_eq!(
                text(
                    OctetStr {
                        data: ptr::null(),
                        len: 1
                    },
                    256
                ),
                Err(INVALID)
            );
            let bad = [255];
            assert_eq!(
                text(
                    OctetStr {
                        data: bad.as_ptr(),
                        len: 1
                    },
                    256
                ),
                Err(TYPE)
            );
            let mut handle = ptr::null_mut();
            assert_eq!(octet_extension_new(&mut handle), OK);
            assert_eq!(octet_extension_new(&mut handle), STATE);
            assert_eq!(octet_extension_free(&mut handle), OK);
            assert!(handle.is_null());
            assert_eq!(octet_extension_free(&mut handle), OK);
            let mut call = OctetCall {
                arguments: json!({"name":"hi"}),
                context: CallContext {
                    terminal: Default::default(),
                    host_context: json!({}),
                },
                result: None,
            };
            let name = OctetStr {
                data: b"name".as_ptr(),
                len: 4,
            };
            let mut out = OctetStr::default();
            assert_eq!(octet_arg_string(&call, name, &mut out), OK);
            assert_eq!(text(out, 256), Ok("hi"));
            let mut owned = b"copied".to_vec();
            assert_eq!(
                octet_result_text(
                    &mut call,
                    OctetStr {
                        data: owned.as_ptr(),
                        len: owned.len()
                    },
                    0
                ),
                OK
            );
            owned.fill(b'x');
            drop(owned);
            assert_eq!(call.result.take().unwrap().text, "copied");
            assert_eq!(octet_arg_integer(&call, name, ptr::null_mut()), INVALID);
            let mut n = 99;
            assert_eq!(octet_arg_integer(&call, name, &mut n), TYPE);
            assert_eq!(n, 0);
        }
    }
}
