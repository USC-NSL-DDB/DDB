#include <iostream>

#include "test1.hpp"

#define DEFINE_DDB_META
#include "ddb/common.h"
#include "ddb/basic.h"
#include "ddb/backtrace.h"

namespace nu {
void test1() {
    DDBMetadata* meta = get_global_ddb_meta();
    std::cout << "hello, world" << std::endl;
    update_ddb_meta(100, 500, nullptr);
    std::cout << "after update_ddb_meta" << std::endl;
    populate_ddb_metadata("ens1f1");
    std::cout << "host: " << ddb_meta.host << std::endl;
    DDBTraceMeta trace_meta;
    get_trace_meta(&trace_meta);
    std::cout << "after get trace meta" << std::endl;
}
}