// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <iostream>
#include <ctime>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

void function_one() {
    int x = 10;
    int y = 20;
    int sum = x + y;
    std::cout << "Function One: Sum = " << sum << std::endl;
}

void function_two() {
    double a = 3.14;
    double b = 2.71;
    double product = a * b;
    std::cout << "Function Two: Product = " << product << std::endl;
}

void function_three() {
    std::string msg = "Hello from function three";
    std::cout << "Function Three: " << msg << std::endl;
    for (int i = 0; i < 3; i++) {
        std::cout << "  Iteration: " << i << std::endl;
    }
}


int main(int argc, char* argv[]) {
    Args args = parse_args(argc, argv);
    auto proc_alias = "hello_world_app";
    if (args.enable_ddb) {
        auto cfg = DDB::Config::get_default("127.0.0.1")
                       .with_alias(proc_alias)
                       .with_logical_group(proc_alias);
        cfg.wait_for_attach = args.wait_for_attach;
        auto connector = DDB::DDBConnector(cfg);
        connector.init();
    }
    
    std::cout << "Starting main function..." << std::endl;
    
    std::cout << "Calling function_one()" << std::endl;
    function_one();
    
    std::cout << "Calling function_two()" << std::endl;
    function_two();
    
    std::cout << "Calling function_three()" << std::endl;
    function_three();
    
    int result = 0;
    for (int i = 1; i <= 10; i++) {
        result += i * i;
    }
    std::cout << "Dummy calculation: Sum of squares from 1 to 10 = " << result << std::endl;
    return 0;
}
