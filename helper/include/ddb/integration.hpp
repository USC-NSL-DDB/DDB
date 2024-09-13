#pragma once

#include <iostream>
#include <string>
#include <csignal>
#include <unistd.h> 

#define DEFINE_DDB_META
#include <ddb/common.hpp>
#include <ddb/basic.hpp>
#include <ddb/service_reporter.hpp>

#include <pthread.h>
#include <stdio.h>

namespace DDB
{
    static int SIGDDBWAIT = 40; // re-use real-time signal for ddb needs

    static inline void block_signal(int sig) {
        sigset_t set;
        sigemptyset(&set);
        sigaddset(&set, sig);
        pthread_sigmask(SIG_BLOCK, &set, NULL);
    }

    static inline void wait_for_signal(int sig) {
        sigset_t set;
        int received_sig;

        sigemptyset(&set);
        sigaddset(&set, sig);

        printf("Process PID: %d. Waiting for signal %d to continue...\n", getpid(), sig);

        // Wait for the signal
        sigwait(&set, &received_sig);

        printf("Received signal %d. Resuming execution.\n", received_sig);
    }

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

            this->discovery = false;
            if (service_reporter_init(&reporter) != 0) {
                std::cerr << "failed to initialize service reporter" << std::endl;
            } else {
                if (report_service(&reporter, &service) != 0) {
                    std::cerr << "failed to report new service" << std::endl;
                } else {
                    this->discovery = true;
                    DDB::DDBConnector::wait_for_debugger();
                }
            }
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

        // sending SIGSTOP to the process to wait for debugger
        static inline void wait_for_debugger() {
            block_signal(SIGDDBWAIT); 
            wait_for_signal(SIGDDBWAIT);
        }
    };
} // namespace DDB
