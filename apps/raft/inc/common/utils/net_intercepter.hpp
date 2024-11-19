#pragma once

#include <grpcpp/client_context.h>
#include <grpcpp/grpcpp.h>
#include <grpcpp/impl/codegen/client_interceptor.h>

#include <chrono>
#include <iostream>
#include <thread>

#include "raft.grpc.pb.h"

namespace rafty {
using grpc::experimental::ClientRpcInfo;
using grpc::experimental::InterceptionHookPoints;
using grpc::experimental::Interceptor;
using grpc::experimental::InterceptorBatchMethods;

enum class NetInterceptionType : uint8_t {
  NETWORK_FAILURE = 0,
  NETWORK_PARTITION = 1,
};

class NetInterceptor : public Interceptor {
public:
  NetInterceptor(ClientRpcInfo *info) : info_(info) {}

  void Intercept(InterceptorBatchMethods *methods) override {
    bool hijack = false;

    if (methods->QueryInterceptionHookPoint(
            InterceptionHookPoints::PRE_SEND_MESSAGE)) {
      auto *meta = methods->GetSendInitialMetadata();
      std::optional<std::string> from;
      std::optional<std::string> to;
      for (const auto &kv : *meta) {
        std::string key = std::string(kv.first.data(), kv.first.length());
        std::string value = std::string(kv.second.data(), kv.second.length());

        if (key == "from") {
          from = value;
        } else if (key == "to") {
          to = value;
        }
      }

      if (from && to) {
        std::lock_guard<std::mutex> lock(mtx);
        switch (type) {
        case NetInterceptionType::NETWORK_FAILURE:
          if (rank2.find(*from) != rank2.end() ||
              rank2.find(*to) != rank2.end()) {
            *methods->GetRecvStatus() = grpc::Status(
                grpc::StatusCode::UNAVAILABLE, "Simulated network failure");
            hijack = true;
          }
          break;
        case NetInterceptionType::NETWORK_PARTITION:
          if (rank1.find(*from) != rank1.end() &&
              rank2.find(*to) != rank2.end()) {
            *methods->GetRecvStatus() = grpc::Status(
                grpc::StatusCode::UNAVAILABLE, "Simulated network failure");
            hijack = true;
          }

          if (rank1.find(*to) != rank1.end() &&
              rank2.find(*from) != rank2.end()) {
            *methods->GetRecvStatus() = grpc::Status(
                grpc::StatusCode::UNAVAILABLE, "Simulated network failure");
            hijack = true;
          }
          break;
        default:
          break;
        }
      }
    }

    if (hijack) {
      methods->Hijack();
    } else {
      methods->Proceed(); // Continue the call
    }
  }

private:
  ClientRpcInfo *info_;

  inline static std::mutex mtx;
  inline static NetInterceptionType type;
  inline static std::set<std::string>
      rank1; // "connected" servers - partition 1
  inline static std::set<std::string>
      rank2; // "disconnected" servers - partition 2

public:
  inline static void disconnect(const std::string &id) {
    std::lock_guard<std::mutex> lock(mtx);
    if (rank1.find(id) != rank1.end()) {
      rank1.erase(id);
      rank2.insert(id);
    }
  }

  inline static void reconnect(const std::string &id) {
    std::lock_guard<std::mutex> lock(mtx);
    if (rank2.find(id) != rank2.end()) {
      rank2.erase(id);
      rank1.insert(id);
    }
  }

  inline static void setup_rank(const std::set<std::string> &ids) {
    std::lock_guard<std::mutex> lock(mtx);
    rank1.clear();
    rank2.clear();
    for (const auto &id : ids) {
      rank1.insert(id);
    }
  }

  inline static void set_type(NetInterceptionType type_) { type = type_; }
};

class NetInterceptorFactory
    : public grpc::experimental::ClientInterceptorFactoryInterface {
public:
  grpc::experimental::Interceptor *
  CreateClientInterceptor(grpc::experimental::ClientRpcInfo *info) override {
    return new NetInterceptor(info);
  }
};

// class NetPartitionInterceptor : public Interceptor {
//     public:
//     NetPartitionInterceptor(ClientRpcInfo* info): info_(info) {}

//     void Intercept(InterceptorBatchMethods* methods) override {
//         bool hijack = false;

//         if (
//             methods->QueryInterceptionHookPoint(
//                 InterceptionHookPoints::PRE_SEND_MESSAGE
//             )
//         ) {
//             auto* meta = methods->GetSendInitialMetadata();
//             std::optional<std::string> from;
//             std::optional<std::string> to;
//             for (const auto& kv : *meta) {
//                 std::string key = std::string(kv.first.data(),
//                 kv.first.length()); std::string value =
//                 std::string(kv.second.data(), kv.second.length());

//                 if (key == "from") {
//                     from = value;
//                 } else if (key == "to") {
//                     to = value;
//                 }
//             }

//             if (from && to) {
//                 std::lock_guard<std::mutex> lock(mtx);
//                 if (rank1.find(*from) != rank1.end() && rank2.find(*to) !=
//                 rank2.end()) {
//                     *methods->GetRecvStatus() =
//                     grpc::Status(grpc::StatusCode::UNAVAILABLE, "Simulated
//                     network failure"); hijack = true;
//                 }

//                 if (rank1.find(*to) != rank1.end() && rank2.find(*from) !=
//                 rank2.end()) {
//                     *methods->GetRecvStatus() =
//                     grpc::Status(grpc::StatusCode::UNAVAILABLE, "Simulated
//                     network failure"); hijack = true;
//                 }
//             }
//         }

//         if (hijack) {
//             methods->Hijack();
//         } else {
//             methods->Proceed();  // Continue the call
//         }
//     }

//     ClientRpcInfo* info_;

//     inline static std::mutex mtx;
//     inline static std::set<std::string> rank1; // "connected" servers -
//     partition 1 inline static std::set<std::string> rank2; // "disconnected"
//     servers - partition 2

//     inline static void disconnect(const std::string& id) {
//         std::lock_guard<std::mutex> lock(mtx);
//         if (rank1.find(id) != rank1.end()) {
//             rank1.erase(id);
//             rank2.insert(id);
//         }
//     }

//     inline static void reconnect(const std::string& id) {
//         std::lock_guard<std::mutex> lock(mtx);
//         if (rank2.find(id) != rank2.end()) {
//             rank2.erase(id);
//             rank1.insert(id);
//         }
//     }

//     inline static void setup_rank(const std::set<std::string>& ids) {
//         std::lock_guard<std::mutex> lock(mtx);
//         rank1.clear();
//         rank2.clear();
//         for (const auto& id : ids) {
//             rank1.insert(id);
//         }
//     }
// };

// class NetPartitionInterceptorFactory : public
// grpc::experimental::ClientInterceptorFactoryInterface { public:
//     grpc::experimental::Interceptor* CreateClientInterceptor(
//         grpc::experimental::ClientRpcInfo* info) override {
//         return new NetPartitionInterceptor(info);
//     }
// };

class ByteCountingInterceptor : public grpc::experimental::Interceptor {
public:
  ByteCountingInterceptor(ClientRpcInfo *info) { this->info_ = info; }

  void Intercept(InterceptorBatchMethods *methods) override {
    if (methods->QueryInterceptionHookPoint(
            InterceptionHookPoints::PRE_SEND_MESSAGE)) {
      auto buffer = methods->GetSerializedSendMessage();
      std::vector<grpc::Slice> slices;
      auto status = buffer->Dump(&slices);
      if (status.ok()) {
        size_t message_size = 0;
        for (const auto &slice : slices) {
          message_size += slice.size();
        }
        total_bytes_sent.fetch_add(message_size);
      }
      total_rpc_count.fetch_add(1);
    }

    methods->Proceed();
  }

  ClientRpcInfo *info_;

  inline static std::atomic<size_t> total_bytes_sent;
  inline static std::atomic<size_t> total_rpc_count;

  inline static size_t get_total_rpc_count() { return total_rpc_count.load(); }

  inline static size_t get_total_bytes_sent() {
    return total_bytes_sent.load();
  }

  inline static void setup_rpc_monitor() {
    total_bytes_sent.store(0);
    total_rpc_count.store(0);
  }
};

class ByteCountingInterceptorFactory
    : public grpc::experimental::ClientInterceptorFactoryInterface {
public:
  grpc::experimental::Interceptor *
  CreateClientInterceptor(grpc::experimental::ClientRpcInfo *info) override {
    return new ByteCountingInterceptor(info);
  }
};
} // namespace rafty