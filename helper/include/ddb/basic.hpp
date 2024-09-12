#pragma once

#include <cstdint>
#include <iostream>

#include <arpa/inet.h>  // For inet_pton and in_addr
#include <netinet/in.h> // For struct in_addr

#include "ddb/common.hpp"

namespace DDB {
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
    init_ddb_meta(meta);
}
}
