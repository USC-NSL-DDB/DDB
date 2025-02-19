#include <iostream>

#include "runtime.hpp"

#include "ddb/common.h"
#include "ddb/basic.h"
#include "ddb/backtrace.h"

namespace nu {
void mod_meta() {
    update_ddb_meta(111, 50050, NULL);
    std::cout << "mod_meta" << std::endl;
}

void op() {
    DDBMetadata* meta = get_global_ddb_meta();
    std::cout << "hello, world" << std::endl;
    update_ddb_meta(100, 500, nullptr);
    std::cout << "after update_ddb_meta" << std::endl;
    mod_meta();
    std::cout << "after mod meta" << std::endl;
    populate_ddb_metadata("ens1f1");
    std::cout << "host: " << ddb_meta.host << std::endl;

    DDBTraceMeta trace_meta;
    get_trace_meta(&trace_meta);
    std::cout << "after get trace meta" << std::endl;
}
}