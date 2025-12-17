#pragma once

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <sys/types.h>
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
#define DDB_HASH_CHUNK_SIZE 8192 // 8KB chunks for partial hash computation

// typedef struct UserData {
//     char key[DDB_MAX_STRING_LEN];       // key name
//     char value[DDB_MAX_STRING_LEN];     // value name
//     struct UserData* next; // pointer to the next key-value pair
// } UserDataMap;

typedef struct {
    uint32_t ip;            // ip address
    char tag[DDB_MAX_STRING_LEN];       // tag name
    pid_t pid;              // process ID
    char hash[DDB_MAX_STRING_LEN];      // hash value of the binary
    char alias[DDB_MAX_STRING_LEN];     // alias name for the binary
    // struct UserDataMap* user_data; // User-defined key-value pairs 
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
        fprintf(stderr, "[DDB Connector] Failed to open service discovery config file\n");
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

#ifdef DEBUG
    printf("[DDB Connector] DDB read from config: address = %s, topic = %s\n", reporter->address, reporter->topic);
#endif

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
        printf("[DDB Connector] Failed to connect MQTT broker, return code %d\n", rc);
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
        fprintf(stderr, "[DDB Connector] Failed to open file %s for hashing\n", filename);
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

static inline int ddb_compute_partial_sha256(const char* filename, char* hash_out) {
    // hash_out should be at least 65 bytes to hold the hex representation
    if (!hash_out) {
        return -1;
    }
    
    FILE* fp = fopen(filename, "rb");
    if (!fp) {
        fprintf(stderr, "[DDB Connector] Failed to open file %s for hashing\n", filename);
        return -1;
    }

    // Get file size
    fseek(fp, 0, SEEK_END);
    long file_size = ftell(fp);
    if (file_size < 0) {
        fclose(fp);
        return -1;
    }
    
    SHA256_CTX ctx;
    sha256_init(&ctx);
    
    // Read first chunk
    fseek(fp, 0, SEEK_SET);
    size_t first_read = (file_size < DDB_HASH_CHUNK_SIZE) ? file_size : DDB_HASH_CHUNK_SIZE;
    unsigned char* first_chunk = (unsigned char*)malloc(first_read);
    if (!first_chunk) {
        fclose(fp);
        return -1;
    }
    
    size_t bytes_read = fread(first_chunk, 1, first_read, fp);
    if (bytes_read != first_read) {
        free(first_chunk);
        fclose(fp);
        return -1;
    }
    sha256_update(&ctx, first_chunk, first_read);
    free(first_chunk);
    
    // Read last chunk if file is large enough
    if (file_size > DDB_HASH_CHUNK_SIZE) {
        size_t last_chunk_size = (file_size - first_read < DDB_HASH_CHUNK_SIZE) ? 
                                 (file_size - first_read) : DDB_HASH_CHUNK_SIZE;
        fseek(fp, -last_chunk_size, SEEK_END);
        
        unsigned char* last_chunk = (unsigned char*)malloc(last_chunk_size);
        if (!last_chunk) {
            fclose(fp);
            return -1;
        }
        
        bytes_read = fread(last_chunk, 1, last_chunk_size, fp);
        if (bytes_read != last_chunk_size) {
            free(last_chunk);
            fclose(fp);
            return -1;
        }
        sha256_update(&ctx, last_chunk, last_chunk_size);
        free(last_chunk);
    }
    
    // Append file size for uniqueness
    sha256_update(&ctx, (const unsigned char*)&file_size, sizeof(file_size));
    
    fclose(fp);
    
    // Finalize hash
    unsigned char hash_binary[32];
    sha256_final(&ctx, hash_binary);
    
    // Convert to hex string
    for (int i = 0; i < 32; i++) {
        sprintf(hash_out + (i * 2), "%02x", hash_binary[i]);
    }
    hash_out[64] = '\0';
    return 0;
}

#ifdef __linux__
static inline int ddb_extract_elf_build_id(const char* filename, char* build_id_out) {
    // build_id_out should be large enough to hold hex representation (typically 41 chars for SHA1-based)
    if (!build_id_out) {
        return -1;
    }
    
    FILE* fp = fopen(filename, "rb");
    if (!fp) {
        return -1;
    }
    
    // Read ELF header
    unsigned char e_ident[16];
    if (fread(e_ident, 1, 16, fp) != 16) {
        fclose(fp);
        return -1;
    }
    
    // Check ELF magic number
    if (e_ident[0] != 0x7f || e_ident[1] != 'E' || e_ident[2] != 'L' || e_ident[3] != 'F') {
        fclose(fp);
        return -1;  // Not an ELF file
    }
    
    int is_64bit = (e_ident[4] == 2);  // 1 = 32-bit, 2 = 64-bit
    int is_little_endian = (e_ident[5] == 1);  // 1 = little endian, 2 = big endian
    
    // Helper macros for reading multi-byte values
    #define READ_U16(data) (is_little_endian ? \
        ((data)[0] | ((data)[1] << 8)) : \
        (((data)[0] << 8) | (data)[1]))
    
    #define READ_U32(data) (is_little_endian ? \
        ((data)[0] | ((data)[1] << 8) | ((data)[2] << 16) | ((data)[3] << 24)) : \
        (((data)[0] << 24) | ((data)[1] << 16) | ((data)[2] << 8) | (data)[3]))
    
    #define READ_U64(data) (is_little_endian ? \
        ((uint64_t)(data)[0] | ((uint64_t)(data)[1] << 8) | ((uint64_t)(data)[2] << 16) | \
         ((uint64_t)(data)[3] << 24) | ((uint64_t)(data)[4] << 32) | ((uint64_t)(data)[5] << 40) | \
         ((uint64_t)(data)[6] << 48) | ((uint64_t)(data)[7] << 56)) : \
        (((uint64_t)(data)[0] << 56) | ((uint64_t)(data)[1] << 48) | ((uint64_t)(data)[2] << 40) | \
         ((uint64_t)(data)[3] << 32) | ((uint64_t)(data)[4] << 24) | ((uint64_t)(data)[5] << 16) | \
         ((uint64_t)(data)[6] << 8) | (uint64_t)(data)[7]))
    
    // Read program header table info
    fseek(fp, 0, SEEK_SET);
    unsigned char header[64];
    size_t header_size = is_64bit ? 64 : 52;
    if (fread(header, 1, header_size, fp) != header_size) {
        fclose(fp);
        return -1;
    }
    
    uint64_t phoff;
    uint16_t phentsize;
    uint16_t phnum;
    
    if (is_64bit) {
        phoff = READ_U64(&header[32]);
        phentsize = READ_U16(&header[54]);
        phnum = READ_U16(&header[56]);
    } else {
        phoff = READ_U32(&header[28]);
        phentsize = READ_U16(&header[42]);
        phnum = READ_U16(&header[44]);
    }
    
    // Search for PT_NOTE segments (type = 4)
    for (uint16_t i = 0; i < phnum; i++) {
        fseek(fp, phoff + i * phentsize, SEEK_SET);
        unsigned char phdr[256];  // Large enough for any program header
        if (fread(phdr, 1, phentsize, fp) != phentsize) {
            continue;
        }
        
        uint32_t p_type = READ_U32(&phdr[0]);
        if (p_type != 4) {  // PT_NOTE = 4
            continue;
        }
        
        uint64_t p_offset, p_filesz;
        if (is_64bit) {
            p_offset = READ_U64(&phdr[8]);
            p_filesz = READ_U64(&phdr[32]);
        } else {
            p_offset = READ_U32(&phdr[4]);
            p_filesz = READ_U32(&phdr[16]);
        }
        
        // Read note section
        if (p_filesz > 65536) {  // Sanity check
            continue;
        }
        
        unsigned char* note_data = (unsigned char*)malloc(p_filesz);
        if (!note_data) {
            continue;
        }
        
        fseek(fp, p_offset, SEEK_SET);
        if (fread(note_data, 1, p_filesz, fp) != p_filesz) {
            free(note_data);
            continue;
        }
        
        // Parse notes
        size_t offset = 0;
        while (offset + 12 <= p_filesz) {
            uint32_t namesz = READ_U32(&note_data[offset]);
            uint32_t descsz = READ_U32(&note_data[offset + 4]);
            uint32_t type = READ_U32(&note_data[offset + 8]);
            offset += 12;
            
            // Align to 4-byte boundary
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
                for (uint32_t j = 0; j < descsz; j++) {
                    sprintf(build_id_out + (j * 2), "%02x", note_data[offset + namesz_aligned + j]);
                }
                build_id_out[descsz * 2] = '\0';
                
                free(note_data);
                fclose(fp);
                
                #undef READ_U16
                #undef READ_U32
                #undef READ_U64
                
                return 0;  // Success
            }
            
            offset += namesz_aligned + descsz_aligned;
        }
        
        free(note_data);
    }
    
    fclose(fp);
    
    #undef READ_U16
    #undef READ_U32
    #undef READ_U64
    
    return -1;  // Build-ID not found
}
#endif

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
        fprintf(stderr, "[DDB Connector] Failed to get self executable path\n");
        return -1;
    }
    
    #ifdef __linux__
    // Try ELF build-ID first (fast path for Linux)
    if (ddb_extract_elf_build_id(exe_path, hash_out) == 0) {
        return 0;
    }
    // If build-ID extraction fails, silently fall through to partial hash
    #endif
    
    // Fallback: use partial hash (cross-platform)
    int result = ddb_compute_partial_sha256(exe_path, hash_out);
    if (result != 0) {
        fprintf(stderr, "[DDB Connector] Failed to compute hash for: %s\n", exe_path);
    }
    return result;
}

static inline int ddb_report_service(DDBServiceReporter* reporter, const DDBServiceInfo* service_info) {
    MQTTClient_message pubmsg = MQTTClient_message_initializer;
    char payload[DDB_PAYLOAD_MAX_LEN];
    
    // payload format: ip:tag:pid:hash=alias[:{<key>=<value>,...}]
    snprintf(payload, DDB_PAYLOAD_MAX_LEN, "%u:%s:%d:%s=%s", 
             service_info->ip, 
             service_info->tag, 
             service_info->pid, 
             service_info->hash, 
             service_info->alias);
    
#ifdef DEBUG
    printf("[DDB Connector] send payload: %s\n", payload);
#endif
    
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
