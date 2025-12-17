#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <fstream>
#include <iostream>
#include <sstream>
#include <iomanip>
#include <limits.h>
#include <vector>

#include <unistd.h>
#include <MQTTClient.h>
#include <string.h>

#include "ddb/common.hpp"
#include "ddb/picosha2.hpp"

namespace DDB {
constexpr static char CLIENTID[] = "s_";
constexpr static char INI_FILEPATH[] = "/tmp/ddb/service_discovery/config";
constexpr static uint8_t QOS = 2;
constexpr static uint32_t TIMEOUT = 10000L;
constexpr static size_t HASH_CHUNK_SIZE = 8192;  // 8KB chunks for partial hash computation

static inline std::string default_ini_filepath() {
    return std::string(INI_FILEPATH);
}

struct ServiceInfo {
    uint32_t ip;        // ip address
    std::string tag;    // tag name
    pid_t pid;          // process ID
    std::string hash;   // hash value of the binary
    std::string alias;  // alias name for the binary
    std::map<std::string, std::string> user_data; // User-defined key-value pairs
};

struct DDBServiceReporter {
    MQTTClient client;       // client for pub
    std::string address;     // broker address
    std::string topic;       // topic for pub
};

static inline int read_config_data(DDBServiceReporter* reporter, const std::string& ini_filepath) {
    std::ifstream file(ini_filepath);
    if (!file.is_open()) {
        std::cerr << "[DDB Connector] Failed to open service discovery config file" << std::endl;
        return -1;
    }

    std::getline(file, reporter->address);
    std::getline(file, reporter->topic);

#ifdef DEBUG
    std::cout << "[DDB Connector] DDB read from config: address = " << reporter->address << ", topic = " << reporter->topic << std::endl;
#endif

    file.close();
    return 0;
}

static inline int service_reporter_init(DDBServiceReporter* reporter, const std::string& ini_filepath = INI_FILEPATH) {
    int rc = read_config_data(reporter, ini_filepath);
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
        printf("[DDB Connector] Failed to connect MQTT broker, return code %d\n", rc);
        return rc;
    }
    return 0;
}

static inline int service_reporter_deinit(DDBServiceReporter* reporter) {
    MQTTClient_disconnect(reporter->client, 10000);
    MQTTClient_destroy(&reporter->client);
    return 0;
}

static inline std::string compute_partial_sha256(const std::string& filename) {
    std::ifstream file(filename, std::ios::binary);
    if (!file) {
        std::cerr << "[DDB Connector] Failed to open file: " << filename << std::endl;
        return "";
    }

    // Get file size
    file.seekg(0, std::ios::end);
    std::streampos file_size_pos = file.tellg();
    size_t file_size = static_cast<size_t>(file_size_pos);
    
    std::vector<unsigned char> buffer;
    buffer.reserve(HASH_CHUNK_SIZE * 2 + sizeof(file_size));
    
    // Read first chunk
    file.seekg(0, std::ios::beg);
    size_t first_read = std::min(HASH_CHUNK_SIZE, file_size);
    std::vector<unsigned char> first_chunk(first_read);
    file.read(reinterpret_cast<char*>(first_chunk.data()), first_read);
    buffer.insert(buffer.end(), first_chunk.begin(), first_chunk.end());
    
    // Read last chunk (if file is large enough)
    if (file_size > HASH_CHUNK_SIZE) {
        size_t last_chunk_size = std::min(HASH_CHUNK_SIZE, file_size - first_read);
        file.seekg(-static_cast<std::streamoff>(last_chunk_size), std::ios::end);
        std::vector<unsigned char> last_chunk(last_chunk_size);
        file.read(reinterpret_cast<char*>(last_chunk.data()), last_chunk_size);
        buffer.insert(buffer.end(), last_chunk.begin(), last_chunk.end());
    }
    
    // Append file size to buffer for uniqueness
    unsigned char size_bytes[sizeof(file_size)];
    memcpy(size_bytes, &file_size, sizeof(file_size));
    buffer.insert(buffer.end(), size_bytes, size_bytes + sizeof(file_size));
    
    return picosha2::hash256_hex_string(buffer);
}

#ifdef __linux__
static inline std::string extract_elf_build_id(const std::string& filename) {
    std::ifstream file(filename, std::ios::binary);
    if (!file) {
        return "";
    }

    // Read ELF header
    unsigned char e_ident[16];
    file.read(reinterpret_cast<char*>(e_ident), 16);
    
    // Check ELF magic number
    if (e_ident[0] != 0x7f || e_ident[1] != 'E' || e_ident[2] != 'L' || e_ident[3] != 'F') {
        return "";  // Not an ELF file
    }

    bool is_64bit = (e_ident[4] == 2);  // 1 = 32-bit, 2 = 64-bit
    bool is_little_endian = (e_ident[5] == 1);  // 1 = little endian, 2 = big endian
    
    // Helper to read multi-byte values with correct endianness
    auto read_u16 = [&](const unsigned char* data) -> uint16_t {
        if (is_little_endian) {
            return data[0] | (data[1] << 8);
        } else {
            return (data[0] << 8) | data[1];
        }
    };
    
    auto read_u32 = [&](const unsigned char* data) -> uint32_t {
        if (is_little_endian) {
            return data[0] | (data[1] << 8) | (data[2] << 16) | (data[3] << 24);
        } else {
            return (data[0] << 24) | (data[1] << 16) | (data[2] << 8) | data[3];
        }
    };
    
    auto read_u64 = [&](const unsigned char* data) -> uint64_t {
        if (is_little_endian) {
            return static_cast<uint64_t>(data[0]) |
                   (static_cast<uint64_t>(data[1]) << 8) |
                   (static_cast<uint64_t>(data[2]) << 16) |
                   (static_cast<uint64_t>(data[3]) << 24) |
                   (static_cast<uint64_t>(data[4]) << 32) |
                   (static_cast<uint64_t>(data[5]) << 40) |
                   (static_cast<uint64_t>(data[6]) << 48) |
                   (static_cast<uint64_t>(data[7]) << 56);
        } else {
            return (static_cast<uint64_t>(data[0]) << 56) |
                   (static_cast<uint64_t>(data[1]) << 48) |
                   (static_cast<uint64_t>(data[2]) << 40) |
                   (static_cast<uint64_t>(data[3]) << 32) |
                   (static_cast<uint64_t>(data[4]) << 24) |
                   (static_cast<uint64_t>(data[5]) << 16) |
                   (static_cast<uint64_t>(data[6]) << 8) |
                   static_cast<uint64_t>(data[7]);
        }
    };

    // Read program header table info
    file.seekg(0, std::ios::beg);
    std::vector<unsigned char> header(is_64bit ? 64 : 52);
    file.read(reinterpret_cast<char*>(header.data()), header.size());
    
    uint64_t phoff;
    uint16_t phentsize;
    uint16_t phnum;
    
    if (is_64bit) {
        phoff = read_u64(&header[32]);
        phentsize = read_u16(&header[54]);
        phnum = read_u16(&header[56]);
    } else {
        phoff = read_u32(&header[28]);
        phentsize = read_u16(&header[42]);
        phnum = read_u16(&header[44]);
    }
    
    // Search for PT_NOTE segments (type = 4)
    for (uint16_t i = 0; i < phnum; ++i) {
        file.seekg(phoff + i * phentsize, std::ios::beg);
        std::vector<unsigned char> phdr(phentsize);
        file.read(reinterpret_cast<char*>(phdr.data()), phentsize);
        
        uint32_t p_type = read_u32(&phdr[0]);
        if (p_type != 4) {  // PT_NOTE = 4
            continue;
        }
        
        uint64_t p_offset, p_filesz;
        if (is_64bit) {
            p_offset = read_u64(&phdr[8]);
            p_filesz = read_u64(&phdr[32]);
        } else {
            p_offset = read_u32(&phdr[4]);
            p_filesz = read_u32(&phdr[16]);
        }
        
        // Read note section
        file.seekg(p_offset, std::ios::beg);
        std::vector<unsigned char> note_data(p_filesz);
        file.read(reinterpret_cast<char*>(note_data.data()), p_filesz);
        
        // Parse notes
        size_t offset = 0;
        while (offset + 12 <= p_filesz) {
            uint32_t namesz = read_u32(&note_data[offset]);
            uint32_t descsz = read_u32(&note_data[offset + 4]);
            uint32_t type = read_u32(&note_data[offset + 8]);
            offset += 12;
            
            // Align namesz and descsz to 4-byte boundary
            uint32_t namesz_aligned = (namesz + 3) & ~3;
            uint32_t descsz_aligned = (descsz + 3) & ~3;
            
            if (offset + namesz_aligned + descsz_aligned > p_filesz) {
                break;
            }
            
            // Check if this is the build-ID note (type = 3, name = "GNU")
            if (type == 3 && namesz == 4 && 
                note_data[offset] == 'G' && note_data[offset + 1] == 'N' && 
                note_data[offset + 2] == 'U' && note_data[offset + 3] == '\0') {
                
                // Found build-ID! Convert to hex string
                std::ostringstream oss;
                oss << std::hex << std::setfill('0');
                for (uint32_t j = 0; j < descsz; ++j) {
                    oss << std::setw(2) << static_cast<unsigned int>(note_data[offset + namesz_aligned + j]);
                }
                return oss.str();
            }
            
            offset += namesz_aligned + descsz_aligned;
        }
    }
    
    return "";  // Build-ID not found
}
#endif

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
        std::cerr << "[DDB Connector] Failed to get self executable path" << std::endl;
        return "";
    }
    
    #ifdef __linux__
    // Try ELF build-ID first (fast path for Linux)
    std::string build_id = extract_elf_build_id(exe_path);
    if (!build_id.empty()) {
        return build_id;
    }
    // If build-ID extraction fails, silently fall through to partial hash
    #endif
    
    // Fallback: use partial hash (cross-platform)
    std::string hash = compute_partial_sha256(exe_path);
    if (hash.empty()) {
        std::cerr << "[DDB Connector] Failed to compute hash for: " << exe_path << std::endl;
    }
    return hash;
}

static inline int report_service(
    DDBServiceReporter* reporter, 
    const ServiceInfo* service_info
) {
    int rc;

    MQTTClient_message pubmsg = MQTTClient_message_initializer;

    std::stringstream ss;

    // payload format:
    // ip:tag:pid:hash=alias:{<key>=<value>,...}
    ss << service_info->ip << ":" << service_info->tag << ":" << service_info->pid << ":" << service_info->hash << "=" << service_info->alias;
    if (!service_info->user_data.empty()) {
        ss << ":{";
        for (const auto& kv : service_info->user_data) {
            ss << kv.first << "=" << kv.second << ",";
        }
        std::string user_data = ss.str();
        user_data.pop_back(); // Remove the last comma
        user_data += "}";
        ss.str(user_data);
    }
    std::string payload = ss.str();

#ifdef DEBUG
    std::cout << "[DDB Connector] send payload: " << payload << std::endl;
#endif

    pubmsg.payload = (void*) payload.c_str();
    // pubmsg.payloadlen = (int) strlen(payload);
    pubmsg.payloadlen = (int) payload.size();
    pubmsg.qos = QOS;
    pubmsg.retained = 0;

    MQTTClient_deliveryToken token;
    MQTTClient_publishMessage(reporter->client, reporter->topic.c_str(), &pubmsg, &token);
    rc = MQTTClient_waitForCompletion(reporter->client, token, TIMEOUT);
    return rc;
}
} // namespace DDB
