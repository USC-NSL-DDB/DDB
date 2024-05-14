#ifndef SERVICE_REPORTER_H
#define SERVICE_REPORTER_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <MQTTClient.h>

#define ADDRESS     "tcp://10.10.2.1:10101"
#define CLIENTID    "service_reporter"
#define T_SERVICE_DISCOVERY "service_discovery/report"
#define QOS         1 // at least once
#define TIMEOUT     10000L

typedef struct {
    uint32_t ip;    // ip address
    char* tag;      // tag name
    pid_t pid;      // process ID
} ServiceInfo;

static MQTTClient client;

static inline int service_reporter_init() {
    MQTTClient_connectOptions conn_opts = MQTTClient_connectOptions_initializer;
    int rc;

    MQTTClient_create(&client, ADDRESS, CLIENTID, MQTTCLIENT_PERSISTENCE_NONE, NULL);
    conn_opts.keepAliveInterval = 20;
    conn_opts.cleansession = 1;

    if ((rc = MQTTClient_connect(client, &conn_opts)) != MQTTCLIENT_SUCCESS) {
        printf("Failed to connect, return code %d\n", rc);
        return rc;
    }
    return 0;
}

static inline int service_reporter_deinit() {
    MQTTClient_disconnect(client, 10000);
    MQTTClient_destroy(&client);
    return 0;
}

static inline int report_service(const ServiceInfo* service_info) {
    int rc;

    MQTTClient_message pubmsg = MQTTClient_message_initializer;

    char payload[256];

    // Format the ServiceInfo struct fields into the buffer
    snprintf(payload, sizeof(payload), "%u:%s:%d",
             service_info->ip, service_info->tag, service_info->pid);

    pubmsg.payload = (void*) payload;
    pubmsg.payloadlen = (int) strlen(payload);
    pubmsg.qos = QOS;
    pubmsg.retained = 0;

    MQTTClient_deliveryToken token;
    MQTTClient_publishMessage(client, T_SERVICE_DISCOVERY, &pubmsg, &token);
    // printf("Waiting for up to %d seconds for publication of %s\n"
    //        "on topic %s for client with ClientID: %s\n",
    //        (int)(TIMEOUT/1000), PAYLOAD, TOPIC, CLIENTID);
    rc = MQTTClient_waitForCompletion(client, token, TIMEOUT);
    printf("Message with delivery token %d delivered\n", token);
    return rc;
}

#ifdef __cplusplus
}
#endif

#endif // SERVICE_REPORTER_H