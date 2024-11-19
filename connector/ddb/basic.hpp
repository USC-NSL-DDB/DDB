#pragma once

#include <cstdint>
#include <iostream>

#include <arpa/inet.h>  // For inet_pton and in_addr
#include <netinet/in.h> // For struct in_addr
#include <ifaddrs.h> // For getifaddrs
#include <unistd.h> // getpid

#include <ddb/common.hpp>

namespace DDB {
static inline std::string uint32_to_ipv4(uint32_t ipv4) {
    struct in_addr addr;
    addr.s_addr = htonl(ipv4);
    char ipv4_str[INET_ADDRSTRLEN];
    inet_ntop(AF_INET, &addr, ipv4_str, INET_ADDRSTRLEN);
    return std::string(ipv4_str);
}

static inline uint32_t ipv4_to_uint32(const std::string& ipv4_addr) {
    struct in_addr addr;  // Structure to store the binary IP address

    // Convert IPv4 address from text to binary form
    if (inet_pton(AF_INET, ipv4_addr.c_str(), &addr) != 1) {
        std::cerr << "Invalid IPv4 address format: " << ipv4_addr << std::endl;
        return 0;
    }

    return ntohl(addr.s_addr);  // Use ntohl to convert to host byte order if necessary
}

static inline void populate_ddb_metadata(const std::string& ipv4_addr) {
    DDBMetadata meta;
    meta.comm_ip = ipv4_to_uint32(ipv4_addr);
    meta.ipv4_str = ipv4_addr;
    meta.pid = getpid();
    init_ddb_meta(meta);
}

// This function will avoid loopback address.
static inline uint32_t get_ipv4_from_local() {
    struct ifaddrs *ifaddr, *ifa;
    int family;

    if (getifaddrs(&ifaddr) == -1) {
        std::cerr << "Error getting network interfaces" << std::endl;
        return 0;
    }

    for (ifa = ifaddr; ifa != nullptr; ifa = ifa->ifa_next) {
        if (ifa->ifa_addr == nullptr)
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

// static inline 

}
