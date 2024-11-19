#include "stdio.h"
#include "test.h"

#include "ddb/common.h"

void mod_meta() {
    update_ddb_meta(111, 50050, NULL);
    printf("mod_meta");
}