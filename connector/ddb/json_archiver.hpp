#pragma once

#include <iostream>
#include <fstream>

#include "cereal/archives/json.hpp"
#include "cereal/types/string.hpp"
#include "ddb/backtrace.hpp"

namespace cereal {
    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBCallerMeta& data) {
        ar(cereal::make_nvp("caller_comm_ip", data.caller_comm_ip));
        ar(cereal::make_nvp("pid", data.pid));
        ar(cereal::make_nvp("tid", data.tid));
    }

    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBCallerContext& data) {
        ar(cereal::make_nvp("pc", data.pc));
        ar(cereal::make_nvp("sp", data.sp));
        ar(cereal::make_nvp("fp", data.fp));
        #ifdef __aarch64__
        ar(cereal::make_nvp("lr", data.lr));
        #endif
    }
    
    template <class Archive>
    inline void serialize(Archive & ar, DDB::DDBTraceMeta& data) {
        ar(cereal::make_nvp("magic", data.magic));
        ar(cereal::make_nvp("meta", data.meta));
        ar(cereal::make_nvp("ctx", data.ctx));
    }
}

namespace DDB
{
    static inline std::string serialize_to_json(const DDBTraceMeta& data) {
        std::ostringstream os(std::ios::binary);
        cereal::JSONOutputArchive archive(os);
        archive(cereal::make_nvp("data", data));
        return os.str();
    }

    static inline DDBTraceMeta deserialize_from_json(const std::string& data) {
        DDBTraceMeta meta;
        std::istringstream is(data, std::ios::binary); 
        cereal::JSONInputArchive archive(is);        
        archive(cereal::make_nvp("data", data));                                  
        return meta;           
    }
} // namespace DDB

