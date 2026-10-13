#ifndef OCTET_EXTENSION_HPP
#define OCTET_EXTENSION_HPP
#include <octet.h>
#include <functional>
#include <initializer_list>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

/* Header-only C++17 authoring wrapper over C ABI 1 / the same Rust runtime.
 * See octet.h for wire scope, thread/ownership contracts and drain exit(70).
 * std::string arguments own their bytes; call itself is callback-scoped.
 * Lambdas/captures live through run. Throwing C++ handlers are contained here,
 * not allowed to unwind across extern "C". No host plugin loading is involved.
 */
namespace octet {
inline octet_str bytes(std::string_view s) { return octet_bytes(s.data(), s.size()); }
class status_error : public std::runtime_error {
public:
    const int32_t status;
    explicit status_error(int32_t code) : std::runtime_error("native SDK status " + std::to_string(code)), status(code) {}
};
inline void check(int32_t status) { if (status != OCTET_OK) throw status_error(status); }
struct result {
    std::string value;
    bool is_error = false;
    static result text(std::string text) { return {std::move(text), false}; }
    static result error(std::string text) { return {std::move(text), true}; }
};
class call {
    octet_call *handle_;
public:
    explicit call(octet_call *handle) : handle_(handle) {}
    call(const call &) = delete;
    call &operator=(const call &) = delete;
    std::string string(std::string_view name) const {
        octet_str out{}; check(octet_arg_string(handle_, bytes(name), &out));
        return std::string(reinterpret_cast<const char *>(out.data), out.len);
    }
    int64_t integer(std::string_view name) const {
        int64_t out = 0; check(octet_arg_integer(handle_, bytes(name), &out)); return out;
    }
    int64_t integer_or(std::string_view name, int64_t fallback) const {
        int64_t out = 0; auto status = octet_arg_integer(handle_, bytes(name), &out);
        if (status == OCTET_MISSING) return fallback;
        check(status); return out;
    }
    bool boolean(std::string_view name) const {
        uint32_t out = 0; check(octet_arg_boolean(handle_, bytes(name), &out)); return out != 0;
    }
    void check_cancelled() const { check(octet_check_cancelled(handle_)); }
    void wait(uint32_t milliseconds) const { check(octet_wait(handle_, milliseconds)); }
};
struct field {
    std::string name;
    uint32_t kind, required;
    size_t max_length;
    int64_t minimum, maximum;
    static field string(std::string name, size_t max_length, bool required = true) {
        return {std::move(name), OCTET_STRING, uint32_t(required), max_length, 0, 0};
    }
    static field integer(std::string name, int64_t minimum, int64_t maximum, bool required = true) {
        return {std::move(name), OCTET_INTEGER, uint32_t(required), 0, minimum, maximum};
    }
    static field boolean(std::string name, bool required = true) {
        return {std::move(name), OCTET_BOOLEAN, uint32_t(required), 0, 0, 0};
    }
};
class extension {
    using handler = std::function<result(call &)>;
    octet_extension *handle_ = nullptr;
    std::vector<std::unique_ptr<handler>> handlers_;
    static int32_t invoke(octet_call *raw, void *user) noexcept {
        try {
            call request(raw);
            auto result = (*static_cast<handler *>(user))(request);
            return octet_result_text(raw, bytes(result.value), uint32_t(result.is_error));
        } catch (const status_error &error) {
            return error.status;
        } catch (...) {
            return octet_result_text(raw, octet_string("native C++ handler threw an exception"), 1);
        }
    }
public:
    extension() { check(octet_extension_new(&handle_)); }
    ~extension() { (void)octet_extension_free(&handle_); }
    extension(const extension &) = delete;
    extension &operator=(const extension &) = delete;
    extension(extension &&) = delete;
    extension &operator=(extension &&) = delete;
    void tool(std::string_view name, std::string_view description, std::initializer_list<field> fields, handler fn) {
        if (!fn) throw std::invalid_argument("tool handler is empty");
        std::vector<octet_field> definitions;
        definitions.reserve(fields.size());
        for (const auto &f : fields) definitions.push_back({bytes(f.name), f.kind, f.required, f.max_length, f.minimum, f.maximum});
        auto owned = std::make_unique<handler>(std::move(fn));
        // Reserve before registration so allocation failure cannot leave a live
        // callback pointing to a destroyed std::function.
        handlers_.reserve(handlers_.size() + 1);
        check(octet_tool_add(handle_, bytes(name), bytes(description), definitions.data(), definitions.size(), &invoke, owned.get()));
        handlers_.push_back(std::move(owned));
    }
    void run() { check(octet_extension_run(handle_)); }
};
}
#endif
