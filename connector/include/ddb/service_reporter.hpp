#pragma once

#include <cstdint>
#include <string>
#include <fstream>
#include <iostream>
#include <sstream>
#include <limits.h>
#include <vector>

#include <unistd.h>
#include <MQTTClient.h>
#include <string.h>

#include "ddb/common.hpp"
#include "ddb/picosha2.h"

namespace DDB {
constexpr static char CLIENTID[] = "s_";
constexpr static char INI_FILEPATH[] = "/tmp/ddb/service_discovery/config";
constexpr static uint8_t QOS = 1;
constexpr static uint32_t TIMEOUT = 10000L;

struct ServiceInfo {
    uint32_t ip;        // ip address
    std::string tag;    // tag name
    pid_t pid;          // process ID
    std::string hash;   // hash value of the binary
    std::string alias;  // alias name for the binary
};

struct DDBServiceReporter {
    MQTTClient client;       // client for pub
    std::string address;     // broker address
    std::string topic;       // topic for pub
};

static inline int read_config_data(DDBServiceReporter* reporter) {
    std::ifstream file(INI_FILEPATH);
    if (!file.is_open()) {
        std::cerr << "Failed to open service discovery config file" << std::endl;
        return -1;
    }

    std::getline(file, reporter->address);
    std::getline(file, reporter->topic);
    std::cout << "read from config: address = " << reporter->address << ", topic = " << reporter->topic << std::endl;

    file.close();
    return 0;
}

static inline int service_reporter_init(DDBServiceReporter* reporter) {
    int rc = read_config_data(reporter);
    if (rc != 0) return rc;

    MQTTClient_connectOptions conn_opts = MQTTClient_connectOptions_initializer;
    MQTTClient_create(
        &reporter->client, 
        reporter->address.c_str(), 
        (CLIENTID + std::to_string(ddb_meta.pid)).c_str(),
        MQTTCLIENT_PERSISTENCE_NONE, 
        NULL
    );
    conn_opts.keepAliveInterval = 20;
    conn_opts.cleansession = 1;

    if ((rc = MQTTClient_connect(reporter->client, &conn_opts)) != MQTTCLIENT_SUCCESS) {
        printf("Failed to connect, return code %d\n", rc);
        return rc;
    }
    return 0;
}

static inline int service_reporter_deinit(DDBServiceReporter* reporter) {
    MQTTClient_disconnect(reporter->client, 10000);
    MQTTClient_destroy(&reporter->client);
    return 0;
}

static inline std::string compute_sha256(const std::string& filename) {
    std::ifstream file(filename, std::ios::binary);
    if (!file) {
        std::cerr << "Failed to open file: " << filename << std::endl;
        return "";
    }

    std::vector<unsigned char> buffer(std::istreambuf_iterator<char>(file), {});
    return picosha2::hash256_hex_string(buffer);
}

static inline std::string get_self_exe_path() {
    char path[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", path, sizeof(path) - 1);
    if (len != -1) {
        path[len] = '\0';
        return std::string(path);
    }
    return "";
}

static inline std::string compute_self_hash() {
    auto exe_path = get_self_exe_path();
    if (exe_path.empty()) {
        std::cerr << "Failed to get self executable path" << std::endl;
        return "";
    }
    return compute_sha256(exe_path);
}

static inline int report_service(
    DDBServiceReporter* reporter, 
    const ServiceInfo* service_info
) {
    int rc;

    MQTTClient_message pubmsg = MQTTClient_message_initializer;

    std::stringstream ss;

    // payload format:
    // ip:tag:pid:hash=alias
    ss << service_info->ip << ":" << service_info->tag << ":" << service_info->pid << ":" << service_info->hash << "=" << service_info->alias;
    std::string payload = ss.str();

    pubmsg.payload = (void*) payload.c_str();
    // pubmsg.payloadlen = (int) strlen(payload);
    pubmsg.payloadlen = (int) payload.size();
    pubmsg.qos = QOS;
    pubmsg.retained = 0;

    MQTTClient_deliveryToken token;
    MQTTClient_publishMessage(reporter->client, reporter->topic.c_str(), &pubmsg, &token);
    rc = MQTTClient_waitForCompletion(reporter->client, token, TIMEOUT);
    printf("Message with delivery token %d delivered\n", token);
    return rc;
}
} // namespace DDB
