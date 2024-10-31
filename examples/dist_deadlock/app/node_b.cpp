#include "example/nodeb.hpp"

using namespace example;

int main(int argc, char **argv) {
  NodeB node = NodeB();
  node.Wait();
  return 0;
}
