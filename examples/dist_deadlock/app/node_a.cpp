#include <string>


#include "example/nodea.hpp"

using namespace example;

void command_loop(NodeA &node_a) {
  while (true) {
    std::string command;
    std::cout << "Enter command: ";
    std::getline(std::cin, command);
    if (command == "invoke") {
      node_a.Invoke(); 
    }
    if (command == "exit") {
      break;
    }
    // Handle other commands here
  }
}

int main(int argc, char **argv) {
  NodeA node_a = NodeA();
  command_loop(node_a);
  node_a.Wait();
  return 0;
}
