#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <MQTTClient.h>

#include "../src/service_reporter.h"


int main(int argc, char* argv[]) {
    int ret;

    ret = service_reporter_init();
    if (ret)
        return -1;

    auto service = ServiceInfo {
        .ip = 168432130, // 10.10.2.2
        .tag = (char*)"dummy_app",
        .pid = getpid()
    };

    ret = report_service(&service);
    if (ret)
        return -1;

    ret = service_reporter_deinit();
    if (ret)
        return -1;

    return 0;
}
