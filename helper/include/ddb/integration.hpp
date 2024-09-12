#pragma once

#include <iostream>
#include <string>
#include <unistd.h> 

#define DEFINE_DDB_META
#include <ddb/common.hpp>
#include <ddb/basic.hpp>
#include <ddb/service_reporter.hpp>

namespace DDB
{
    class DDBConnector {
     public:
        inline void deinit_discovery() {
            int ret_val = service_reporter_deinit(&reporter);
            if (ret_val)
                std::cerr << "failed to deinit service reporter" << std::endl;
        }

        inline void deinit() {
            if (this->discovery)
                this->deinit_discovery();
        }

        inline void init_discovery(const std::string& tag = "proc") {
            auto service = ServiceInfo {
                .ip = DDB::ddb_meta.comm_ip,
                .tag = tag,
                .pid = DDB::ddb_meta.pid
            };

            if (service_reporter_init(&reporter) != 0) {
                std::cerr << "failed to initialize service reporter" << std::endl;
            } else {
                if (report_service(&reporter, &service) != 0) {
                    std::cerr << "failed to report new service" << std::endl;
                }
            }
            this->discovery = true;
        }

        inline void init(const std::string& ipv4, bool enable_discovery = true) {
            populate_ddb_metadata(ipv4);
            if (enable_discovery)
                this->init_discovery();
            this->discovery = enable_discovery;
            std::cout << "ddb initialized. meta = { pid = " 
                    << DDB::ddb_meta.pid << ", comm_ip = " 
                    << DDB::ddb_meta.comm_ip << ", ipv4_str =" 
                    << DDB::ddb_meta.ipv4_str << " }" 
                    << std::endl;
        }

        DDBConnector() = default;
        ~DDBConnector() {
            this->deinit();
        }

     private:
        DDBServiceReporter reporter;
        bool discovery;
    };
} // namespace DDB
