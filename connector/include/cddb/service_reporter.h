#pragma once

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <unistd.h>
#include <MQTTClient.h>

#include "cddb/common.h"
#include "cddb/sha256.h"

#ifdef __cplusplus
extern "C" {
#endif

#define DDB_CLIENTID "s_"
#define DDB_INI_FILEPATH "/tmp/ddb/service_discovery/config"
#define DDB_QOS 2
#define DDB_TIMEOUT 10000L
#define DDB_MAX_STRING_LEN 1024
#define DDB_PAYLOAD_MAX_LEN 4096

#define DDB_HASH_LEN 65 // SHA-256 hash length in hex (64 chars + null terminator)

typedef struct {
    uint32_t ip;            // ip address
    char tag[DDB_MAX_STRING_LEN];       // tag name
    pid_t pid;              // process ID
    char hash[DDB_MAX_STRING_LEN];      // hash value of the binary
    char alias[DDB_MAX_STRING_LEN];     // alias name for the binary
} DDBServiceInfo;

typedef struct {
    MQTTClient client;                   // client for pub
    char address[DDB_MAX_STRING_LEN];    // broker address
    char topic[DDB_MAX_STRING_LEN];      // topic for pub
} DDBServiceReporter;

static inline const char* ddb_default_ini_filepath() {
    return DDB_INI_FILEPATH;
}

static inline int ddb_read_config_data(DDBServiceReporter* reporter, const char* ini_filepath) {
    FILE* file = fopen(ini_filepath, "r");
    if (!file) {
        fprintf(stderr, "Failed to open service discovery config file\n");
        return -1;
    }

    if (fgets(reporter->address, DDB_MAX_STRING_LEN, file) == NULL) {
        fclose(file);
        return -1;
    }
    // Remove newline character
    reporter->address[strcspn(reporter->address, "\n")] = 0;

    if (fgets(reporter->topic, DDB_MAX_STRING_LEN, file) == NULL) {
        fclose(file);
        return -1;
    }
    // Remove newline character
    reporter->topic[strcspn(reporter->topic, "\n")] = 0;

    printf("read from config: address = %s, topic = %s\n", reporter->address, reporter->topic);

    fclose(file);
    return 0;
}

static inline int ddb_service_reporter_init(DDBServiceReporter* reporter, const char* ini_filepath) {
    if (ini_filepath == NULL) {
        ini_filepath = DDB_INI_FILEPATH;
    }
    
    int rc = ddb_read_config_data(reporter, ini_filepath);
    if (rc != 0) return rc;

    MQTTClient_connectOptions conn_opts = MQTTClient_connectOptions_initializer;
    char client_id[DDB_MAX_STRING_LEN];
    snprintf(client_id, DDB_MAX_STRING_LEN, "%s%d", DDB_CLIENTID, ddb_meta.pid);
    
    MQTTClient_create(
        &reporter->client, 
        reporter->address, 
        client_id,
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

static inline int ddb_service_reporter_deinit(DDBServiceReporter* reporter) {
    MQTTClient_disconnect(reporter->client, 10000);
    MQTTClient_destroy(&reporter->client);
    return 0;
}

static inline int ddb_compute_sha256(const char* filename, char* hash_out) {
    // hash_out should be at least 65 bytes to hold the hex representation of the SHA-256 hash
    if (!hash_out) { // SHA-256 in hex is 64 chars + null terminator
        return -1;
    }
    
    FILE* fp = fopen(filename, "rb");
    if (!fp) {
        fprintf(stderr, "Failed to open file %s for hashing\n", filename);
        return -1;
    }

    SHA256_CTX ctx;
    sha256_init(&ctx);

    unsigned char buffer[4096];
    size_t bytes_read;
    while ((bytes_read = fread(buffer, 1, sizeof(buffer), fp)) > 0) {
        sha256_update(&ctx, buffer, bytes_read);
    }

    fclose(fp);

    unsigned char hash_binary[32];
    sha256_final(&ctx, hash_binary);

    // Convert binary hash to hex string
    for (int i = 0; i < 32; i++) {
        sprintf(hash_out + (i * 2), "%02x", hash_binary[i]);
    }
    hash_out[64] = '\0';
    return 0;
}

static inline int ddb_get_self_exe_path(char* path_out, size_t path_out_size) {
    ssize_t len = readlink("/proc/self/exe", path_out, path_out_size - 1);
    if (len != -1) {
        path_out[len] = '\0';
        return 0;
    }
    return -1;
}

static inline int ddb_compute_self_hash(char* hash_out) {
    char exe_path[PATH_MAX];
    if (ddb_get_self_exe_path(exe_path, PATH_MAX) != 0) {
        fprintf(stderr, "Failed to get self executable path\n");
        return -1;
    }
    return ddb_compute_sha256(exe_path, hash_out);
}

static inline int ddb_report_service(DDBServiceReporter* reporter, const DDBServiceInfo* service_info) {
    MQTTClient_message pubmsg = MQTTClient_message_initializer;
    char payload[DDB_PAYLOAD_MAX_LEN];
    
    // payload format: ip:tag:pid:hash=alias
    snprintf(payload, DDB_PAYLOAD_MAX_LEN, "%u:%s:%d:%s=%s", 
             service_info->ip, 
             service_info->tag, 
             service_info->pid, 
             service_info->hash, 
             service_info->alias);
    
    printf("send payload: %s\n", payload);
    
    pubmsg.payload = payload;
    pubmsg.payloadlen = (int)strlen(payload);
    pubmsg.qos = DDB_QOS;
    pubmsg.retained = 0;
    
    MQTTClient_deliveryToken token;
    MQTTClient_publishMessage(reporter->client, reporter->topic, &pubmsg, &token);
    return MQTTClient_waitForCompletion(reporter->client, token, DDB_TIMEOUT);
}

#ifdef __cplusplus
}
#endif
