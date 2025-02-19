#include <stdio.h>

#include "test.h"

#define DEFINE_DDB_META
#include "ddb/common.h"
#include "ddb/basic.h"
#include "ddb/backtrace.h"


int main() {
    DDBMetadata* meta = get_global_ddb_meta();
    printf("hello, world\n");
    update_ddb_meta(100, 500, NULL);
    printf("after update_ddb_meta\n");
    mod_meta();
    printf("after mod meta\n");
    populate_ddb_metadata("ens1f1");
    printf("after populate\n");
    printf("%s\n", ddb_meta.host);

    DDBTraceMeta trace_meta;
    get_trace_meta(&trace_meta);
    printf("after get trace meta");

    return 0;
}