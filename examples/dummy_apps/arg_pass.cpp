#include <iostream>

void performOperations(int num1, double num2, char ch) {
    // Perform operations on the arguments
    int sum = num1 + static_cast<int>(num2);
    char nextChar = ch + 1;

    // Print the results
    std::cout << "Sum: " << sum << std::endl;
    std::cout << "Next character: " << nextChar << std::endl;
}

int main(int argc, char* argv[]) {
    // Check if the correct number of arguments are provided
    if (argc != 4) {
        std::cout << "Usage: ./program <integer> <double> <character>" << std::endl;
        return 1;
    }

    // Parse the arguments
    int num1 = std::stoi(argv[1]);
    double num2 = std::stod(argv[2]);
    char ch = argv[3][0];

    // Call the function to perform operations
    performOperations(num1, num2, ch);

    return 0;
}
