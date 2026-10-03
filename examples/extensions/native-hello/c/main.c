#include <octet.h>
#define TRY(expr) do { int32_t status = (expr); if (status != OCTET_OK) return status; } while (0)

static int32_t hello(octet_call *call, void *user) {
    (void)user;
    octet_str name = {0}; int64_t delay = 0;
    TRY(octet_arg_string(call, octet_string("name"), &name));
    int32_t status = octet_arg_integer(call, octet_string("delay_ms"), &delay);
    if (status != OCTET_OK && status != OCTET_MISSING) return status;
    TRY(octet_wait(call, (uint32_t)delay));
    /* 256 Unicode scalars can occupy up to 1024 UTF-8 bytes. */
    char greeting[1032];
    memcpy(greeting, "Hello, ", 7); memcpy(greeting + 7, name.data, name.len);
    greeting[7 + name.len] = '!';
    return octet_result_text(call, octet_bytes(greeting, name.len + 8), 0);
}
int main(void) {
    octet_extension *extension = NULL;
    octet_field fields[] = {octet_string_field("name", 256, 1), octet_integer_field("delay_ms", 0, 5000, 0)};
    TRY(octet_extension_new(&extension));
    int32_t status = octet_tool_add(extension, octet_string("hello"), octet_string("Return a local greeting"), fields, 2, hello, NULL);
    if (status == OCTET_OK) status = octet_extension_run(extension);
    (void)octet_extension_free(&extension);
    return status;
}
