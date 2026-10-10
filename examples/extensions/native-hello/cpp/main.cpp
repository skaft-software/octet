#include <octet.hpp>
#include <iostream>
int main() {
    try {
        octet::extension extension;
        extension.tool("hello", "Return a local greeting", {
            octet::field::string("name", 256), octet::field::integer("delay_ms", 0, 5000, false)
        }, [](octet::call &call) {
            call.wait(static_cast<uint32_t>(call.integer_or("delay_ms", 0)));
            return octet::result::text("Hello, " + call.string("name") + "!");
        });
        extension.run();
    } catch (const std::exception &error) {
        std::cerr << error.what() << '\n'; return 1;
    }
}
