#pragma once

#include <condition_variable>
#include <mutex>
#include <optional>
#include <queue>
#include <stdexcept>

namespace toolings {
/**
 * A thread-safe message queue implementation.
 * The behavior is similar to golang's channel implemenatation.
 *
 * @tparam T The type of the message to be stored in the queue.
 *
 * @note The queue can be unbuffered by setting the buffer_size to 0.
 *
 * If the queue is unbuffered, the producer (enqueue) will block until the
 * consumer (dequeue) is ready. If the queue is buffered, the producer (enqueue)
 * will block if the internal queue is full.
 *
 * The consumer (dequeue) will block if the internal queue is empty.
 * If you want to avoid blocking, you can use try_dequeue() which will return an
 * optional value. In this case, if the queue is empty, it will return
 * std::nullopt.
 *
 */
template <typename T> class MessageQueue {
public:
  explicit MessageQueue(size_t buffer_size = 0)
      : buffer_size_(buffer_size), closed(false) {}

  ~MessageQueue() { close(); }

  // Disable copy and assignment
  MessageQueue(const MessageQueue &) = delete;
  MessageQueue &operator=(const MessageQueue &) = delete;

  // Move constructor
  MessageQueue(MessageQueue &&other) noexcept { *this = std::move(other); }

  // Move assignment operator
  MessageQueue &operator=(MessageQueue &&other) noexcept {
    if (this != &other) {
      std::lock_guard<std::mutex> lock(mutex_);
      queue_ = std::move(other.queue_);
      buffer_size_ = other.buffer_size_;
      // closed_ = other.closed_;

      other.buffer_size_ = 0;
      other.closed = false;
    }
    return *this;
  }

  void enqueue(T value) {
    std::unique_lock<std::mutex> lock(mutex_);

    prod_ready_.wait(lock, [this] {
      if (buffer_size_ == 0) {
        return queue_.empty() || this->closed;
      }
      return queue_.size() < buffer_size_ || this->closed;
    });

    if (this->closed) {
      return;
    }

    this->queue_.push(std::move(value));
    this->con_ready_.notify_all();
    if (buffer_size_ == 0) {
      prod_ready_.wait(lock, [this] { return queue_.empty() || this->closed; });
    }
  }

  T dequeue() {
    std::unique_lock<std::mutex> lock(mutex_);
    con_ready_.wait(lock, [this] { return !queue_.empty() || this->closed; });
    if (this->closed) {
      return T{};
    }
    T value = std::move(queue_.front());
    queue_.pop();
    prod_ready_.notify_all();
    return value;
  }

  std::optional<T> try_dequeue() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (queue_.empty()) {
      return std::nullopt;
    }
    T value = std::move(queue_.front());
    queue_.pop();
    return value;
  }

  void close() {
    std::lock_guard<std::mutex> lock(mutex_);
    this->closed = true;
    prod_ready_.notify_all();
    con_ready_.notify_all();
  }

  bool empty() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return queue_.empty();
  }

  size_t size() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return queue_.size();
  }

private:
  size_t buffer_size_;
  bool closed;
  mutable std::mutex mutex_;
  std::queue<T> queue_;
  std::condition_variable prod_ready_;
  std::condition_variable con_ready_;
};
} // namespace toolings
