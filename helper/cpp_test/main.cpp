#include <iostream>
#include <string>

#include "ddb/integration.hpp"
#include "ddb/backtrace.hpp"

#include <type_traits>

using namespace DDB;

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

    DDB::DDBConnector connector;
    connector.init("10.10.1.2", true);

    std::cout << DDB::ddb_meta.comm_ip << " " << DDB::ddb_meta.pid << " " << DDB::ddb_meta.ipv4_str << std::endl;

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