#pragma once

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cddb/backtrace.h"  // Assuming this has C compatible definitions

#ifdef __cplusplus
extern "C" {
#endif

// Serializes DDBTraceMeta to a string, caller must free the returned string
static inline char* ddb_serialize_to_str(const DDBTraceMeta* data) {
    char* buffer = NULL;
    size_t len = 0;
    
    #ifdef __aarch64__
    len = snprintf(NULL, 0, "%d,%d,%d,%d,%lu,%lu,%lu,%lu", 
                 data->magic, 
                 data->meta.caller_comm_ip,
                 data->meta.pid, 
                 data->meta.tid,
                 data->ctx.pc, 
                 data->ctx.sp, 
                 data->ctx.fp,
                 data->ctx.lr);
    #else
    len = snprintf(NULL, 0, "%lu,%d,%d,%d,%lu,%lu,%lu", 
                 data->magic, 
                 data->meta.caller_comm_ip,
                 data->meta.pid, 
                 data->meta.tid,
                 data->ctx.pc, 
                 data->ctx.sp, 
                 data->ctx.fp);
    #endif
    
    buffer = (char*)malloc(len + 1);
    if (!buffer) {
        return NULL;
    }
    
    #ifdef __aarch64__
    snprintf(buffer, len + 1, "%d,%d,%d,%d,%lu,%lu,%lu,%lu", 
             data->magic, 
             data->meta.caller_comm_ip,
             data->meta.pid, 
             data->meta.tid,
             data->ctx.pc, 
             data->ctx.sp, 
             data->ctx.fp,
             data->ctx.lr);
    #else
    snprintf(buffer, len + 1, "%lu,%d,%d,%d,%lu,%lu,%lu", 
             data->magic, 
             data->meta.caller_comm_ip,
             data->meta.pid, 
             data->meta.tid,
             data->ctx.pc, 
             data->ctx.sp, 
             data->ctx.fp);
    #endif
    
    return buffer;
}

// Deserializes a string to DDBTraceMeta
// Returns 0 on success, -1 on failure
static inline int ddb_deserialize_from_str(const char* data, DDBTraceMeta* trace) {
    if (!data || !trace) {
        return -1;
    }
    
    int result;
    
    #ifdef __aarch64__
    result = sscanf(data, "%d,%d,%d,%d,%lu,%lu,%lu,%lu", 
                  &trace->magic, 
                  &trace->meta.caller_comm_ip,
                  &trace->meta.pid, 
                  &trace->meta.tid,
                  &trace->ctx.pc, 
                  &trace->ctx.sp, 
                  &trace->ctx.fp,
                  &trace->ctx.lr);
    
    if (result != 8) {
        return -1;
    }
    #else
    result = sscanf(data, "%ld,%d,%d,%d,%lu,%lu,%lu", 
                  &trace->magic, 
                  &trace->meta.caller_comm_ip,
                  &trace->meta.pid, 
                  &trace->meta.tid,
                  &trace->ctx.pc, 
                  &trace->ctx.sp, 
                  &trace->ctx.fp);
    
    if (result != 7) {
        return -1;
    }
    #endif
    
    return 0;
}

#ifdef __cplusplus
}
#endif
