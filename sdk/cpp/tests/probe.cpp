#include <octet.hpp>
#include <cassert>
#include <iostream>
int main() {
    try {
        octet::extension ext;
        bool refused = false;
        try { ext.tool("bad", "Empty callback", {}, {}); }
        catch (const std::invalid_argument &) { refused = true; }
        assert(refused);
        auto captured = std::make_shared<std::string>("capture survives registration");
        ext.tool("probe", "Exercise C++ ownership and exception containment", {
            octet::field::string("mode", 32), octet::field::string("text", OCTET_MAX_TEXT_BYTES, false)
        }, [captured](octet::call &call) {
            assert(*captured == "capture survives registration");
            auto mode = call.string("mode");
            if (mode == "cancel") { call.wait(5000); return octet::result::text("not cancelled"); }
            if (mode == "throw") throw std::runtime_error("must not cross C ABI");
            if (mode == "status") throw octet::status_error(OCTET_TYPE);
            if (mode == "error") return octet::result::error("domain failure");
            if (mode == "empty") return octet::result::text("");
            return octet::result::text(call.string("text"));
        });
        captured.reset();
        refused = false;
        try { ext.tool("probe", "Duplicate callback", {}, [](octet::call &) { return octet::result::text("bad"); }); }
        catch (const octet::status_error &) { refused = true; }
        assert(refused);
        ext.run();
    } catch (const std::exception &error) { std::cerr << error.what() << '\n'; return 1; }
}
