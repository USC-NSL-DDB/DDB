#include <ddb/integration.hpp>
#include "example/nodeb.hpp"

using namespace example;

int main(int argc, char **argv) {
  auto ddb_config = DDB::Config::get_default("10.10.1.2");
  auto connector = DDB::DDBConnector(ddb_config);
  connector.init();
  NodeB node = NodeB();
  node.Wait();
  return 0;
}
