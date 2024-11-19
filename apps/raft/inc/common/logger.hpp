#pragma once

#include <iostream>

#include "spdlog/async.h"
#include "spdlog/cfg/env.h"
#include "spdlog/common.h"
#include "spdlog/sinks/basic_file_sink.h"
#include "spdlog/sinks/stdout_color_sinks.h"
#include "spdlog/spdlog.h"

__attribute__((constructor)) static void initialize_spdlog_thread_pool() {
  spdlog::init_thread_pool(8192, 3);
}

namespace rafty {
namespace utils {
class logger {
private:
  std::shared_ptr<spdlog::async_logger> _logger;

public:
  logger(const logger &) = delete;
  logger &operator=(const logger &) = delete;

  inline static std::unique_ptr<logger> get_logger(uint64_t id) {
    std::string name = std::format("rafty_node_{}", id);
    return std::make_unique<logger>(name);
  }

  // inline static logger &get_instance() {
  //   static logger instance;
  //   return instance;
  // }

  inline void set_level(spdlog::level::level_enum level) {
    _logger->set_level(level);
  }

  template <typename... Args>
  inline void trace(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    this->_logger->trace(fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  inline void debug(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    this->_logger->debug(fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  inline void info(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    // log(level::info, fmt, std::forward<Args>(args)...);
    this->_logger->info(fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  inline void warn(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    this->_logger->warn(fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  inline void error(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    this->_logger->error(fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  inline void critical(spdlog::format_string_t<Args...> fmt, Args &&...args) {
    this->_logger->critical(fmt, std::forward<Args>(args)...);
  }

  template <typename T> inline void trace(const T &msg) {
    this->_logger->trace(msg);
  }

  template <typename T> inline void debug(const T &msg) {
    this->_logger->debug(msg);
  }

  template <typename T> inline void info(const T &msg) {
    this->_logger->info(msg);
  }

  template <typename T> inline void warn(const T &msg) {
    this->_logger->warn(msg);
  }

  template <typename T> inline void error(const T &msg) {
    this->_logger->error(msg);
  }

  template <typename T> inline void critical(const T &msg) {
    this->_logger->critical(msg);
  }

  // You generally don't need to use this function
  // unless you want to use spdlog's advanced features
  inline std::shared_ptr<spdlog::logger> get_raw() { return this->_logger; }

public:
  logger(const std::string &name);
  ~logger() = default;
};

inline logger::logger(const std::string &name) {
  spdlog::cfg::load_env_levels();
  try {
    auto spdlogger =
        std::dynamic_pointer_cast<spdlog::async_logger>(spdlog::get(name));
    if (!spdlogger) {
      auto stdout_sink =
          std::make_shared<spdlog::sinks::stdout_color_sink_mt>();
      auto file_sink = std::make_shared<spdlog::sinks::basic_file_sink_mt>(
          std::format("logs/{}.log", name), true);
      std::vector<spdlog::sink_ptr> sinks{stdout_sink, file_sink};
      auto logger = std::make_shared<spdlog::async_logger>(
          name, sinks.begin(), sinks.end(), spdlog::thread_pool(),
          spdlog::async_overflow_policy::block);
      spdlog::register_logger(logger);
      spdlog::flush_every(std::chrono::seconds(1));
      this->_logger = std::move(logger);
    } else {
      this->_logger = std::move(spdlogger);
    }
  }

  catch (const spdlog::spdlog_ex &ex) {
    std::cout << "Log init failed: " << ex.what() << std::endl;
  }
}
} // namespace utils
} // namespace rafty
