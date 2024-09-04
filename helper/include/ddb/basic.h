#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ifaddrs.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/types.h>
#include <sys/socket.h>

#include "ddb/common.h"

// Function to populate the DDB metadata given an interface name
static inline void populate_ddb_metadata(const char *ifa_name) {
    struct ifaddrs *ifaddr, *ifa;
    int family;

    // Get the list of network interfaces
    if (getifaddrs(&ifaddr) == -1) {
        perror("getifaddrs");
        goto free_ifaddrs;  // Jump to the cleanup section
    }

    // Iterate through the linked list of interfaces
    for (ifa = ifaddr; ifa != NULL; ifa = ifa->ifa_next) {
        if (ifa->ifa_addr == NULL) {
            continue;  // Skip if the address is NULL
        }

        family = ifa->ifa_addr->sa_family;

        // Check if the interface name matches and if it's an IPv4 address
        if (strcmp(ifa->ifa_name, ifa_name) == 0 && family == AF_INET) {  // AF_INET for IPv4
            struct sockaddr_in *ipv4 = (struct sockaddr_in *)ifa->ifa_addr;
            DDBMetadata meta;
            meta.comm_ip = htonl(ipv4->sin_addr.s_addr);   // Convert IP address
            meta.comm_port = htons(ipv4->sin_port);        // Convert port
            inet_ntop(AF_INET, &(ipv4->sin_addr), meta.host, NI_MAXHOST);  // Convert IP to string
            init_ddb_meta(&meta);

            break;  // Exit the loop once the desired interface is found
        }
    }

free_ifaddrs:
    // Cleanup the linked list
    if (ifaddr != NULL) {
        freeifaddrs(ifaddr);
    }

    // Check if the IP was successfully initialized
    // if (ddb_meta.comm_ip == 0) {
    //     printf("ifa IP address is not initialized.\n");
    // }
}

#ifdef __cplusplus
}
#endif
