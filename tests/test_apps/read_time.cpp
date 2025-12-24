// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <iostream>
#include <chrono>
#include <thread>
#include <ctime>

int main() {
    while (true) {
        auto now = std::chrono::system_clock::now();
        std::time_t now_time = std::chrono::system_clock::to_time_t(now);
        std::cout << std::ctime(&now_time);
        std::this_thread::sleep_for(std::chrono::seconds(1));
    }
    return 0;
}
