#include <gtest/gtest.h>
#include <thread>

#include "toolings/msg_queue.hpp"

using namespace toolings;

TEST(MessageQueueTest, SimpleEnqueueAndDequeue) {
  MessageQueue<int> mq(100);
  EXPECT_TRUE(mq.empty());
  mq.enqueue(1);
  EXPECT_FALSE(mq.empty());
  mq.dequeue();
  EXPECT_TRUE(mq.empty());
}

TEST(MessageQueueTest, EnqueueAndDequeueMultipleMessages) {
  MessageQueue<int> mq(100);
  mq.enqueue(10);
  mq.enqueue(20);
  mq.enqueue(30);

  EXPECT_EQ(mq.dequeue(), 10);
  EXPECT_EQ(mq.dequeue(), 20);
  EXPECT_EQ(mq.dequeue(), 30);
  EXPECT_TRUE(mq.empty());
}

TEST(MessageQueueTest, EnqueueUnbufferedQueue) {
  MessageQueue<int> mq(0);
  std::chrono::duration<double> dur;
  std::thread producer([&]() {
    auto prod_start = std::chrono::high_resolution_clock::now();
    mq.enqueue(99);
    auto prod_end = std::chrono::high_resolution_clock::now();
    dur = prod_end - prod_start;
  });

  std::this_thread::sleep_for(std::chrono::seconds(1));
  auto val = mq.dequeue();
  producer.join();

  EXPECT_EQ(val, 99);
  EXPECT_GE(dur.count(), 1.0); // Ensure that it waited for at least 1s
}

TEST(MessageQueueTest, DequeueUnbufferedQueue) {
  MessageQueue<int> mq(0);
  std::chrono::duration<double> dur;
  std::thread consumer([&]() {
    auto cons_start = std::chrono::high_resolution_clock::now();
    mq.dequeue();
    auto cons_end = std::chrono::high_resolution_clock::now();
    dur = cons_end - cons_start;
  });

  std::this_thread::sleep_for(std::chrono::seconds(1));
  mq.enqueue(99);
  consumer.join();

  EXPECT_GE(dur.count(), 1.0); // Ensure that it waited for at least 1s
}

TEST(MessageQueueTest, BufferedEnqueueBlocking) {
  MessageQueue<int> mq(1);
  std::chrono::duration<double> first_elem_dur;
  std::chrono::duration<double> second_elem_dur;
  std::thread producer([&]() {
    auto prod_start = std::chrono::high_resolution_clock::now();
    mq.enqueue(99);
    auto prod_end = std::chrono::high_resolution_clock::now();
    first_elem_dur = prod_end - prod_start;
    SCOPED_TRACE("Producer (1st): " + std::to_string(first_elem_dur.count()));

    prod_start = std::chrono::high_resolution_clock::now();
    mq.enqueue(100);
    prod_end = std::chrono::high_resolution_clock::now();
    second_elem_dur = prod_end - prod_start;
    SCOPED_TRACE("Producer (2nd): " + std::to_string(second_elem_dur.count()));
  });

  std::this_thread::sleep_for(std::chrono::seconds(1));
  auto val_1 = mq.dequeue();
  EXPECT_EQ(val_1, 99);
  auto val_2 = mq.dequeue();
  EXPECT_EQ(val_2, 100);
  producer.join();

  EXPECT_LT(first_elem_dur.count(),
            0.01); // shouldn't block, heuristically check if less than 10ms
  EXPECT_GE(second_elem_dur.count(),
            1.0); // Ensure that it waited for at least 1s
}

TEST(MessageQueueTest, BufferedDequeueBlocking) {
  MessageQueue<int> mq(1);
  std::chrono::duration<double> first_elem_dur;
  std::chrono::duration<double> second_elem_dur;
  std::thread consumer([&]() {
    auto cons_start = std::chrono::high_resolution_clock::now();
    mq.dequeue();
    auto cons_end = std::chrono::high_resolution_clock::now();
    first_elem_dur = cons_end - cons_start;
    SCOPED_TRACE("Consumer (1st): " + std::to_string(first_elem_dur.count()));

    cons_start = std::chrono::high_resolution_clock::now();
    mq.dequeue();
    cons_end = std::chrono::high_resolution_clock::now();
    second_elem_dur = cons_end - cons_start;
    SCOPED_TRACE("Consumer (2nd): " + std::to_string(second_elem_dur.count()));
  });

  std::this_thread::sleep_for(std::chrono::seconds(1));
  mq.enqueue(99);
  std::this_thread::sleep_for(std::chrono::seconds(2));
  mq.enqueue(100);
  consumer.join();

  EXPECT_GE(first_elem_dur.count(), 1.0);
  EXPECT_GE(second_elem_dur.count(), 2.0);
}

// TEST(MessageQueueTest, DequeueBlocksUntilMessageIsAvailable) {
//     MessageQueue<int> mq;

// std::thread producer([&mq]() {
//     std::this_thread::sleep_for(std::chrono::milliseconds(100));
//     mq.enqueue(99);
// });

//     auto start = std::chrono::high_resolution_clock::now();
//     int message = mq.dequeue();
//     auto end = std::chrono::high_resolution_clock::now();

//     std::chrono::duration<double> elapsed = end - start;

//     EXPECT_EQ(message, 99);
//     EXPECT_GE(elapsed.count(), 0.1);  // Ensure that it waited for at least
//     100ms

//     producer.join();
// }

// TEST(MessageQueueTest, EmptyQueue) {
//     MessageQueue<int> mq;
//     EXPECT_TRUE(mq.empty());
//     mq.enqueue(1);
//     EXPECT_FALSE(mq.empty());
//     mq.dequeue();
//     EXPECT_TRUE(mq.empty());
// }