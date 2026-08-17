#include "engine/boundedTaskPool.h"

#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <mutex>

namespace {

using routesocks::runtime::BoundedTaskPool;

/**
 * 用阻塞长任务占满连接池，并验证独立控制池仍能立即执行 DNS/SOCKS 控制工作。
 * 若执行池再次被合并，本测试会在严格超时内失败而不是长时间挂起。
 */
void VerifyProtocolPoolsRemainIndependent() {
  std::mutex gateMutex;
  std::condition_variable gateCondition;
  bool releaseConnections = false;
  std::atomic<int> failures{0};
  BoundedTaskPool connectionPool(2, 4, [&failures]() { ++failures; });
  BoundedTaskPool controlPool(1, 4, [&failures]() { ++failures; });
  BoundedTaskPool datagramPool(1, 4, [&failures]() { ++failures; });
  connectionPool.Start();
  controlPool.Start();
  datagramPool.Start();

  for (int index = 0; index < 2; ++index) {
    const bool submitted = connectionPool.Submit([&]() {
      std::unique_lock<std::mutex> lock(gateMutex);
      gateCondition.wait(lock, [&]() { return releaseConnections; });
    });
    assert(submitted);
  }

  std::mutex completionMutex;
  std::condition_variable completionCondition;
  int completedProtocolTasks = 0;
  const auto signalProtocolCompletion = [&]() {
    {
      std::lock_guard<std::mutex> lock(completionMutex);
      ++completedProtocolTasks;
    }
    completionCondition.notify_one();
  };
  assert(controlPool.Submit(signalProtocolCompletion));
  assert(datagramPool.Submit(signalProtocolCompletion));
  {
    std::unique_lock<std::mutex> lock(completionMutex);
    assert(completionCondition.wait_for(
        lock, std::chrono::milliseconds(200),
        [&]() { return completedProtocolTasks == 2; }));
  }

  {
    std::lock_guard<std::mutex> lock(gateMutex);
    releaseConnections = true;
  }
  gateCondition.notify_all();
  connectionPool.Stop();
  controlPool.Stop();
  datagramPool.Stop();
  assert(failures.load() == 0);
}

} // namespace

/** 运行有界执行池的宿主机回归测试；任一断言失败返回非零进程状态。 */
int main() {
  VerifyProtocolPoolsRemainIndependent();
  return 0;
}
