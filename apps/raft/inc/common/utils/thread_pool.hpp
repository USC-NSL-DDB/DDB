#pragma once

#include <condition_variable>
#include <functional>
#include <future>
#include <mutex>
#include <queue>
#include <stop_token>
#include <thread>
#include <vector>

namespace rafty {
namespace utils {

class ThreadPool {
public:
  explicit ThreadPool(size_t num_threads) {
    for (size_t i = 0; i < num_threads; ++i) {
      workers.emplace_back([this](std::stop_token stoken) {
        while (!stoken.stop_requested()) {
          std::function<void()> task;
          {
            std::unique_lock lock(queue_mutex);
            condition.wait(lock, stoken, [this] { return !tasks.empty(); });
            if (tasks.empty())
              continue;
            task = std::move(tasks.front());
            tasks.pop();
          }
          task(); // Execute the task
        }
      });
    }
  }

  // Submit a task to the pool and return a future result
  template <class F, class... Args>
  auto enqueue(F &&f, Args &&...args)
      -> std::future<std::invoke_result_t<F, Args...>> {
    using return_type = std::invoke_result_t<F, Args...>;

    auto task = std::make_shared<std::packaged_task<return_type()>>(
        std::bind(std::forward<F>(f), std::forward<Args>(args)...));

    std::future<return_type> result = task->get_future();

    {
      std::unique_lock lock(queue_mutex);
      tasks.emplace([task]() { (*task)(); });
    }

    condition.notify_one(); // Wake up a thread to handle the task
    return result;
  }

  ~ThreadPool() {
    // Request all threads to stop and clean up
    for (auto &worker : workers) {
      worker.request_stop(); // Stop all worker threads
    }
    condition.notify_all(); // Wake up any waiting threads
  }

private:
  std::vector<std::jthread> workers;
  std::queue<std::function<void()>> tasks;

  std::mutex queue_mutex;
  std::condition_variable_any condition;
};
} // namespace utils
} // namespace rafty
