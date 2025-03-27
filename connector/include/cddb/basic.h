#pragma once

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <arpa/inet.h>  // For inet_pton and in_addr
#include <netinet/in.h> // For struct in_addr
#include <ifaddrs.h>    // For getifaddrs
#include <unistd.h>     // getpid

#include "cddb/common.h"

#ifdef __cplusplus
extern "C" {
#endif

// Convert uint32_t to IPv4 string
static inline char* ddb_uint32_to_ipv4(uint32_t ipv4) {
    struct in_addr addr;
    addr.s_addr = htonl(ipv4);
    char* ipv4_str = (char*) malloc(INET_ADDRSTRLEN);
    if (ipv4_str == NULL) {
        fprintf(stderr, "Memory allocation failed\n");
        return NULL;
    }
    inet_ntop(AF_INET, &addr, ipv4_str, INET_ADDRSTRLEN);
    return ipv4_str;
}

// Convert IPv4 string to uint32_t
static inline uint32_t ddb_ipv4_to_uint32(const char* ipv4_addr) {
    struct in_addr addr;  // Structure to store the binary IP address

    // Convert IPv4 address from text to binary form
    if (inet_pton(AF_INET, ipv4_addr, &addr) != 1) {
        fprintf(stderr, "Invalid IPv4 address format: %s\n", ipv4_addr);
        return 0;
    }

    return ntohl(addr.s_addr);  // Use ntohl to convert to host byte order if necessary
}

// Populate DDB metadata
static inline void populate_ddb_metadata(const char* ipv4_addr) {
    DDBMetadata meta;
    meta.comm_ip = ddb_ipv4_to_uint32(ipv4_addr);
    strncpy(meta.ipv4_str, ipv4_addr, sizeof(meta.ipv4_str) - 1);
    meta.ipv4_str[sizeof(meta.ipv4_str) - 1] = '\0';
    meta.pid = getpid();
    init_ddb_meta(&meta);
}

// Get IPv4 from local (avoiding loopback)
static inline uint32_t ddb_get_ipv4_from_local(void) {
    struct ifaddrs *ifaddr, *ifa;
    int family;

    if (getifaddrs(&ifaddr) == -1) {
        fprintf(stderr, "Error getting network interfaces\n");
        return 0;
    }

    for (ifa = ifaddr; ifa != NULL; ifa = ifa->ifa_next) {
        if (ifa->ifa_addr == NULL)
            continue;

        family = ifa->ifa_addr->sa_family;
        if (family == AF_INET) {
            struct sockaddr_in *addr = (struct sockaddr_in *)ifa->ifa_addr;
            uint32_t ip = ntohl(addr->sin_addr.s_addr);

            // Check if the IP is not a loopback (127.0.0.1)
            if (ip != 0x7F000001) {
                freeifaddrs(ifaddr);
                return ip;
            }
        }
    }

    freeifaddrs(ifaddr);
    return 0;
}

#ifdef __cplusplus
}
#endif
