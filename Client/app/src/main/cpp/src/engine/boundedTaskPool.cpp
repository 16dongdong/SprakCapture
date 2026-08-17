#include "engine/boundedTaskPool.h"

#include <stdexcept>
#include <utility>

namespace routesocks::runtime {

BoundedTaskPool::BoundedTaskPool(std::size_t workerCount,
                                 std::size_t maximumQueuedTasks,
                                 std::function<void()> failureCallback)
    : workerCount_(workerCount), maximumQueuedTasks_(maximumQueuedTasks),
      failureCallback_(std::move(failureCallback)) {
  if (workerCount_ == 0 || maximumQueuedTasks_ == 0 || !failureCallback_) {
    throw std::invalid_argument("有界任务池配置无效");
  }
}

BoundedTaskPool::~BoundedTaskPool() { Stop(); }

void BoundedTaskPool::Start() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (running_ || !threads_.empty()) {
      throw std::logic_error("有界任务池已经启动");
    }
    running_ = true;
  }
  try {
    for (std::size_t index = 0; index < workerCount_; ++index) {
      threads_.emplace_back(&BoundedTaskPool::RunWorker, this);
    }
  } catch (...) {
    Stop();
    throw;
  }
}

bool BoundedTaskPool::Submit(std::function<void()> task) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!running_ || !task || tasks_.size() >= maximumQueuedTasks_) {
    return false;
  }
  tasks_.push_back(std::move(task));
  taskCondition_.notify_one();
  return true;
}

void BoundedTaskPool::WaitIdle() {
  std::unique_lock<std::mutex> lock(mutex_);
  idleCondition_.wait(
      lock, [this]() { return tasks_.empty() && executingTasks_ == 0; });
}

void BoundedTaskPool::Stop() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    running_ = false;
  }
  taskCondition_.notify_all();
  for (std::thread &thread : threads_) {
    if (thread.joinable()) {
      thread.join();
    }
  }
  threads_.clear();
  {
    std::lock_guard<std::mutex> lock(mutex_);
    tasks_.clear();
    executingTasks_ = 0;
  }
  idleCondition_.notify_all();
}

void BoundedTaskPool::RunWorker() noexcept {
  while (true) {
    std::function<void()> task;
    {
      std::unique_lock<std::mutex> lock(mutex_);
      taskCondition_.wait(lock,
                          [this]() { return !running_ || !tasks_.empty(); });
      if (tasks_.empty()) {
        return;
      }
      task = std::move(tasks_.front());
      tasks_.pop_front();
      ++executingTasks_;
    }
    try {
      task();
    } catch (...) {
      // 单个协议会话的分配或解析异常只能失败该任务，不能终止整个 Native 进程。
      try {
        failureCallback_();
      } catch (...) {
        // 异常回调位于 noexcept 线程边界，二次异常不能破坏执行池的资源回收。
      }
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      --executingTasks_;
      if (tasks_.empty() && executingTasks_ == 0) {
        idleCondition_.notify_all();
      }
    }
  }
}

} // namespace routesocks::runtime
