#pragma once

#include <iostream>
#include <fstream>

#include "cereal/archives/json.hpp"
#include "cereal/types/string.hpp"
#include "ddb/backtrace.h"

namespace cereal {
    template <class Archive>
    inline void serialize(Archive & ar, DDBCallerMeta& data) {
        ar(cereal::make_nvp("caller_comm_ip", data.caller_comm_ip));
        ar(cereal::make_nvp("pid", data.pid));
    }

    template <class Archive>
    inline void serialize(Archive & ar, DDBCallerContext& data) {
        ar(cereal::make_nvp("rbp", data.rbp));
        ar(cereal::make_nvp("rip", data.rip));
        ar(cereal::make_nvp("rsp", data.rsp));
    }
    
    template <class Archive>
    inline void serialize(Archive & ar, DDBTraceMeta& data) {
        ar(cereal::make_nvp("magic", data.magic));
        ar(cereal::make_nvp("meta", data.meta));
        ar(cereal::make_nvp("ctx", data.ctx));
        // ar(data.magic);
        // ar(data.meta);
        // ar(data.ctx);
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

