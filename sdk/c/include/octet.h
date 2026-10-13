#ifndef OCTET_EXTENSION_H
#define OCTET_EXTENSION_H
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#ifdef __cplusplus
extern "C" {
#endif

/* Source SDK 0.8.2, C ABI 1, feature-negotiated extension API 0.4.
 * This library authors a supervised executable, not an in-host C plugin.
 *
 * OWNERSHIP / SAFETY:
 * - Create with a NULL handle slot; free(&handle) nulls it. No handle aliases,
 *   double frees, forged/stale pointers or concurrent handle operations.
 * - All non-NULL pointers must be valid/aligned for their declared length/type.
 *   NULL+0 text/field arrays are allowed. NULL+positive length is refused.
 *   Lengths are bytes; strings are UTF-8, not NUL-terminated. Embedded NUL is data.
 *   Bounds are checked before reading. Arbitrary pointer accessibility cannot be
 *   validated; invalid foreign memory is a caller bug, not a recoverable status.
 * - Tool names/descriptions/field names are copied during add. Callback/user_data
 *   are borrowed through run and invoked on one worker thread, never concurrently.
 * - While run is active do not call handle operations, including free/add/run.
 *   Only callback-thread accessors may use the borrowed call handle. Do not retain
 *   call handles or borrowed argument strings past callback return.
 * - Results are copied during result_text. Supply exactly one result and return
 *   OK. A non-OK callback status yields an inspectable tool failure; CANCELLED
 *   yields -32800. Callbacks must not throw, longjmp or unwind across the ABI.
 * - No Rust unwind crosses exported functions. PANIC reports a caught panic;
 *   allocation failure / invalid memory / abort cannot be recovered this way.
 * - run consumes registrations and can run only once per process. Free the handle
 *   after return. Graceful shutdown cancels/joins calls, replies and returns.
 *   EOF cancels/joins without a shutdown reply. After 500 ms an uncooperative
 *   callback causes process exit(70), never a return with borrowed data live.
 *   The host's process supervisor remains the final authority and time bound.
 *
 * This bounded lane supports static tools, flat string/integer/boolean C inputs,
 * text results/errors and cancellation only. Unsupported hooks/UI/commands/
 * flags and required features fail initialize; no reverse host services exist.
 */
typedef struct OctetExtension octet_extension;
typedef struct OctetCall octet_call;
typedef struct { const uint8_t *data; size_t len; } octet_str;
typedef struct {
    octet_str name;
    uint32_t kind;       /* OCTET_STRING / INTEGER / BOOLEAN */
    uint32_t required;   /* exactly 0 or 1 */
    size_t max_length;   /* string Unicode scalar count; global 128 KiB byte cap */
    int64_t minimum;     /* integer only, portable JSON range */
    int64_t maximum;     /* integer only */
} octet_field;
typedef int32_t (*octet_callback)(octet_call *, void *user_data);
enum { OCTET_OK=0, OCTET_INVALID=1, OCTET_MISSING=2, OCTET_TYPE=3,
       OCTET_BOUNDS=4, OCTET_STATE=5, OCTET_PANIC=6, OCTET_CANCELLED=7,
       OCTET_RUNTIME=8 };
enum { OCTET_STRING=1, OCTET_INTEGER=2, OCTET_BOOLEAN=3 };
#define OCTET_MAX_TEXT_BYTES ((size_t)131072)

uint32_t octet_abi_version(void);
int32_t octet_extension_new(octet_extension **empty_out);
int32_t octet_extension_free(octet_extension **handle);
/* At most 256 tools, 64 fields/tool. Tool name 1..64 bytes; description 1..4096;
 * field name 1..256. Duplicate/invalid registration is atomic and refused. */
int32_t octet_tool_add(octet_extension *, octet_str name, octet_str description,
                      const octet_field *, size_t field_count,
                      octet_callback, void *user_data);
int32_t octet_extension_run(octet_extension *);
/* Valid outputs are zeroed on any failure. MISSING means absent, not null.
 * A present JSON null or wrong type yields TYPE. Required/type/length/range
 * validation runs before invoking the callback. */
int32_t octet_arg_string(const octet_call *, octet_str name, octet_str *borrowed_out);
int32_t octet_arg_integer(const octet_call *, octet_str name, int64_t *out);
int32_t octet_arg_boolean(const octet_call *, octet_str name, uint32_t *out);
int32_t octet_check_cancelled(const octet_call *);
int32_t octet_wait(const octet_call *, uint32_t milliseconds); /* 0..60000 */
int32_t octet_result_text(octet_call *, octet_str text, uint32_t is_error); /* 0/1 */

static inline octet_str octet_bytes(const void *data, size_t len) {
    octet_str value = { (const uint8_t *)data, len }; return value;
}
/* Convenience for author-owned NUL-terminated names/literals only. */
static inline octet_str octet_string(const char *text) {
    return octet_bytes(text, text ? strlen(text) : 0);
}
static inline octet_field octet_string_field(const char *name, size_t max_length, uint32_t required) {
    octet_field field = { octet_string(name), OCTET_STRING, required, max_length, 0, 0 }; return field;
}
static inline octet_field octet_integer_field(const char *name, int64_t min, int64_t max, uint32_t required) {
    octet_field field = { octet_string(name), OCTET_INTEGER, required, 0, min, max }; return field;
}
static inline octet_field octet_boolean_field(const char *name, uint32_t required) {
    octet_field field = { octet_string(name), OCTET_BOOLEAN, required, 0, 0, 0 }; return field;
}
#ifdef __cplusplus
}
#endif
#endif
