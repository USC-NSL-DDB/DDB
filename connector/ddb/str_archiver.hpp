#pragma once

#include <iostream>
#include <sstream>
#include <string>
#include <stdexcept>

#include <ddb/backtrace.hpp>

namespace DDB {
    static inline std::string serialize_to_str(const DDBTraceMeta& data) {
        std::ostringstream oss;
        oss << data.magic << ','
            << data.meta.caller_comm_ip << ','
            << data.meta.pid << ','
            << data.meta.tid << ','
            << data.ctx.pc << ','
            << data.ctx.sp << ','
            << data.ctx.fp;
        #ifdef __aarch64__
            oss << ',' << data.ctx.lr;
        #endif
        return oss.str();
    }

    static inline DDBTraceMeta deserialize_from_str(const std::string& data) {
        DDBTraceMeta trace;
        std::istringstream iss(data);
        char comma;  // to consume commas between values

        if (
            !(iss >> trace.magic >> comma
            >> trace.meta.caller_comm_ip >> comma
            >> trace.meta.pid >> comma
            >> trace.meta.tid >> comma
            >> trace.ctx.pc >> comma
            >> trace.ctx.sp >> comma
            >> trace.ctx.fp)
        ) {
            throw std::invalid_argument("Failed to deserialize caller context.");
        }

        #ifdef __aarch64__
        if (!(iss >> comma >> trace.ctx.lr)) {
            throw std::invalid_argument("Failed to deserialize lr value.");
        }
        #endif

        return trace;
    }
}

// namespace DDB
// {
// } // namespace DDB
