#pragma once

#include <iostream>
#include <sstream>
#include <string>
#include <stdexcept>

#include "ddb/backtrace.h"

namespace DDB{
    static inline std::string serialize_to_str(const DDBTraceMeta& data) {
        std::ostringstream oss;
        oss << data.magic << ','
            << data.meta.caller_comm_ip << ','
            << data.meta.pid << ','
            << data.ctx.rip << ','
            << data.ctx.rsp << ','
            << data.ctx.rbp;
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
            >> trace.ctx.rip >> comma
            >> trace.ctx.rsp >> comma
            >> trace.ctx.rbp)
        ) {
            throw std::invalid_argument("Failed to deserialize the input string.");
        }

        return trace;
    }
}

// namespace DDB
// {
// } // namespace DDB
