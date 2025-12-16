#include <iostream>

void inner_frame(int arg1, int arg2) {
    std::cout << "This is inner frame" << std::endl;
    std::cout << "arg1: " << arg1 << ", arg2: " << arg2 << std::endl;
}

int add(int a, int b) {
    return a + b;
}

int main() {
    std::cout << "Hello, World!" << std::endl;
    int a = 1;
    int b = 2;
    inner_frame(a, b);

    int result = add(a, b);
    std::cout << "result: " << result << std::endl;

    return 0;
}
