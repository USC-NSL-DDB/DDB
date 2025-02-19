#include <stdlib.h>
#include <MQTTClient.h>

#include "ddb/integration.hpp"

constexpr const char* ip_addr = "127.0.0.1";

int main(int argc, char* argv[]) {
    auto ddb_config = DDB::Config::get_default(ip_addr)
        .with_alias("dummy_app_alias")
        .with_tag("dummy_app_tag")
        .with_ini_filepath("/tmp/ddb/service_discovery/config");

    std::cout << ddb_config.to_string() << std::endl;
    
    auto connector = DDB::DDBConnector(ddb_config);
    connector.init();
    return 0;
}
