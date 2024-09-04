#include <iostream>

#include "runtime.hpp"
#include "ddb/common.h"

void mod_meta() {
    update_ddb_meta(111, 50050, NULL);
    std::cout << "mod_meta" << std::endl;
}