#include <iostream>
#include <string>

#define DEFINE_DDB_META
#include "ddb/common.h"
#include "ddb/basic.h"
#include "ddb/backtrace.h"

#include <type_traits>

class Invoker {
    public:
    Invoker() = default;
    ~Invoker() = default;

    void invoke_func(const std::string& input) {
        std::cout << "invoked: " << input << std::endl;
    }

    std::string invoke_func_rt(const std::string& input) {
        std::cout << "invoked_rt: " << input << std::endl;
        return "invoked + " + input;
    }
};

int main() {
    Invoker invoker;
    std::string test = "test";
    std::string handler = "handler";

    // auto myRPCCallable = [&]() -> std::string {
    //     return invoker.invoke_func_rt(handler);
    // };

    DDB::Backtrace::extraction([&]() {
        DDBTraceMeta meta;
        meta.magic = 12345;
        std::cout << "extractor: " << test << std::endl;
        return meta;
    }, [&]() {
        // invoker.invoke_func(handler);
        invoker.invoke_func_rt(handler);
    });

    auto rt = DDB::Backtrace::extraction<std::string>([&]() {
        DDBTraceMeta meta;
        meta.magic = 12345;
        std::cout << "extractor: " << test << std::endl;
        return meta;
    }, [&]() {
        // invoker.invoke_func(handler);
        return invoker.invoke_func_rt(handler);
    });

    std::cout << "rt: " << rt << std::endl;

    // auto rt = DDB::Backtrace::extraction<std::string>([&]() -> DDBTraceMeta {
    //     DDBTraceMeta meta;
    //     meta.magic = 12345;
    //     std::cout << "extractor: " << test << std::endl;
    //     return meta;
    // },
    //     myRPCCallable
    // );

    return 0;
}