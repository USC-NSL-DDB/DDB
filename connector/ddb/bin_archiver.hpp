#pragma once

#include <iostream>

#include "cereal/archives/binary.hpp"
#include "ddb/backtrace.hpp"

namespace cereal {
    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBCallerMeta& data) {
        ar(data.caller_comm_ip);
        ar(data.pid);
        ar(data.tid);
    }

    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBCallerContext& data) {
        ar(data.pc);
        ar(data.sp);
        ar(data.fp);
        #ifdef __aarch64__
        ar(data.lr);
        #endif
    }
    
    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBTraceMeta& data) {
        ar(data.magic);
        ar(data.meta);
        ar(data.ctx);
    }
}

namespace DDB
{
    static inline std::string serialize_to_bin(const DDBTraceMeta& data) {
        std::ostringstream os(std::ios::binary);
        cereal::BinaryOutputArchive archive(os);
        archive(data);
        return os.str();
    }

    static inline DDBTraceMeta deserialize_from_bin(const std::string& data) {
        DDBTraceMeta meta;
        std::istringstream is(data, std::ios::binary); 
        cereal::BinaryInputArchive archive(is);        
        archive(meta);                                 
        return meta;           
    }
} // namespace DDB

