#include <octet.h>
#include <assert.h>
#include <stdlib.h>
#include <stdio.h>
#define S(text) octet_string(text)
_Static_assert(sizeof(octet_str) == sizeof(void *) + sizeof(size_t), "slice layout");
_Static_assert(offsetof(octet_field, name) == 0, "field layout");

static int32_t probe(octet_call *call, void *user) {
    assert(user && *(int *)user == 42);
    octet_str mode = {0};
    assert(octet_arg_string(call, S("mode"), &mode) == OCTET_OK);
    if (mode.len == 6 && memcmp(mode.data,"cancel",6) == 0) {
        return octet_wait(call,5000);
    }
    if (mode.len == 4 && memcmp(mode.data,"omit",4) == 0) return OCTET_OK;
    if (mode.len == 5 && memcmp(mode.data,"error",5) == 0) return octet_result_text(call,S("domain failure"),1);
    if (mode.len == 5 && memcmp(mode.data,"empty",5) == 0) return octet_result_text(call,octet_bytes(NULL,0),0);
    if (mode.len == 9 && memcmp(mode.data,"duplicate",9) == 0) {
        assert(octet_result_text(call,S("first"),0) == OCTET_OK);
        return octet_result_text(call,S("second"),0);
    }
    octet_str out = { (const uint8_t *)"dirty",5 }; int64_t integer = 99; uint32_t boolean = 99;
    assert(octet_arg_string(call,S("missing"),&out) == OCTET_MISSING && out.data == NULL && out.len == 0);
    assert(octet_arg_integer(call,S("mode"),&integer) == OCTET_TYPE && integer == 0);
    assert(octet_arg_boolean(call,S("mode"),&boolean) == OCTET_TYPE && boolean == 0);
    assert(octet_arg_string(call,octet_bytes(NULL,1),&out) == OCTET_INVALID);
    assert(octet_arg_string(call,octet_bytes(NULL,SIZE_MAX),&out) == OCTET_BOUNDS);
    assert(octet_arg_string(NULL,S("mode"),&out) == OCTET_INVALID);
    assert(octet_arg_string(call,S("mode"),NULL) == OCTET_INVALID);
    assert(octet_arg_integer(call,S("mode"),NULL) == OCTET_INVALID);
    assert(octet_arg_boolean(call,S("mode"),NULL) == OCTET_INVALID);
    assert(octet_wait(call,60001) == OCTET_BOUNDS);
    assert(octet_check_cancelled(call) == OCTET_OK);
    assert(octet_result_text(NULL,S("result"),0) == OCTET_INVALID);
    assert(octet_result_text(call,octet_bytes(NULL,1),0) == OCTET_INVALID);
    assert(octet_result_text(call,octet_bytes(NULL,SIZE_MAX),0) == OCTET_BOUNDS);
    const uint8_t invalid_utf8[] = {0xff};
    assert(octet_result_text(call,octet_bytes(invalid_utf8,1),0) == OCTET_TYPE);
    assert(octet_result_text(call,S("bad flag"),2) == OCTET_INVALID);
    assert(octet_arg_string(call,S("text"),&out) == OCTET_OK);
    if (mode.len == 5 && memcmp(mode.data,"typed",5) == 0) {
        assert(octet_arg_integer(call,S("number"),&integer) == OCTET_OK && integer == -9007199254740991LL);
        assert(octet_arg_boolean(call,S("flag"),&boolean) == OCTET_OK && boolean == 1);
    }
    char *owned = malloc(out.len ? out.len : 1); assert(owned);
    memcpy(owned,out.data,out.len);
    assert(octet_result_text(call,octet_bytes(owned,out.len),0) == OCTET_OK);
    memset(owned,'x',out.len); free(owned); /* result must already own its bytes */
    assert(octet_result_text(call,S("duplicate"),0) == OCTET_STATE);
    return OCTET_OK;
}
static int32_t unused(octet_call *call, void *user) { (void)call; (void)user; return OCTET_OK; }
int main(int argc, char **argv) {
    (void)argv;
    assert(octet_abi_version() == 1);
    assert(octet_extension_new(NULL) == OCTET_INVALID);
    assert(octet_extension_free(NULL) == OCTET_INVALID);
    assert(octet_extension_run(NULL) == OCTET_INVALID);
    assert(octet_check_cancelled(NULL) == OCTET_INVALID);
    assert(octet_wait(NULL,0) == OCTET_INVALID);
    octet_extension *ext = NULL;
    assert(octet_extension_new(&ext) == OCTET_OK);
    assert(octet_extension_new(&ext) == OCTET_STATE);
    assert(octet_tool_add(ext,S("a"),S("test"),NULL,0,NULL,NULL) == OCTET_INVALID);
    assert(octet_tool_add(ext,octet_bytes(NULL,1),S("test"),NULL,0,unused,NULL) == OCTET_INVALID);
    assert(octet_tool_add(ext,octet_bytes(NULL,SIZE_MAX),S("test"),NULL,0,unused,NULL) == OCTET_BOUNDS);
    assert(octet_tool_add(ext,S("a"),S("test"),NULL,1,unused,NULL) == OCTET_INVALID);
    assert(octet_tool_add(ext,S("a"),S("test"),NULL,SIZE_MAX,unused,NULL) == OCTET_BOUNDS);
    octet_field invalid = octet_integer_field("value",10,0,1);
    assert(octet_tool_add(ext,S("a"),S("test"),&invalid,1,unused,NULL) == OCTET_INVALID);
    invalid = octet_integer_field("value",0,INT64_MAX,1);
    assert(octet_tool_add(ext,S("a"),S("test"),&invalid,1,unused,NULL) == OCTET_BOUNDS);
    invalid = octet_string_field("value",SIZE_MAX,1);
    assert(octet_tool_add(ext,S("a"),S("test"),&invalid,1,unused,NULL) == OCTET_BOUNDS);
    assert(octet_extension_free(&ext) == OCTET_OK && ext == NULL);
    assert(octet_extension_free(&ext) == OCTET_OK);
    if (argc == 1) { fputs("C ownership/null/length checks passed\n",stderr); return 0; }
    assert(octet_extension_new(&ext) == OCTET_OK);
    /* Registration buffers can be overwritten and freed immediately. */
    char *name = malloc(6), *description = malloc(16), *field_name = malloc(5);
    assert(name && description && field_name);
    memcpy(name,"probe",6); memcpy(description,"Lifetime checks",16); memcpy(field_name,"mode",5);
    octet_field fields[] = {octet_string_field(field_name,32,1),octet_string_field("text",OCTET_MAX_TEXT_BYTES,0),octet_integer_field("number",-9007199254740991LL,9007199254740991LL,0),octet_boolean_field("flag",0)};
    int user = 42;
    assert(octet_tool_add(ext,S(name),S(description),fields,4,probe,&user) == OCTET_OK);
    memset(name,'x',5); memset(description,'x',15); memset(field_name,'x',4);
    free(name); free(description); free(field_name); memset(fields,0,sizeof(fields));
    int32_t status = octet_extension_run(ext);
    assert(octet_extension_run(ext) == OCTET_STATE);
    assert(octet_extension_free(&ext) == OCTET_OK && ext == NULL);
    return status;
}
