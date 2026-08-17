#pragma once

#include <condition_variable>
#include <cstddef>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>
#include <vector>

namespace routesocks::runtime {

/**
 * 为一种阻塞任务提供独立、有界的执行资源。
 *
 * TCP 转发、SOCKS 控制握手和 UDP 数据报具有完全不同的阻塞时长，不能共享同一组
 * 工作线程；否则少量长连接即可让 DNS 与 UDP
 * 永久排队。执行池只负责调度和生命周期，
 * 具体协议失败由调用方提供的异常回调转换为运行指标。
 */
class BoundedTaskPool {
public:
  /**
   * 保存线程数、最大排队数和异常回调，构造阶段不创建线程。
   * workerCount 和 maximumQueuedTasks 必须大于零；任务抛出时调用
   * failureCallback。
   */
  BoundedTaskPool(std::size_t workerCount, std::size_t maximumQueuedTasks,
                  std::function<void()> failureCallback);

  /** 析构时幂等停止并等待全部线程退出，保证没有任务继续引用所属运行时。 */
  ~BoundedTaskPool();

  BoundedTaskPool(const BoundedTaskPool &) = delete;
  BoundedTaskPool &operator=(const BoundedTaskPool &) = delete;

  /**
   * 创建固定数量的工作线程；线程创建失败会回收已经创建的线程并继续抛出原异常。
   * 同一实例只允许在停止状态启动。
   */
  void Start();

  /**
   * 提交一个任务；执行池未启动、正在停止或队列已满时返回
   * false，任务不会被执行。
   */
  bool Submit(std::function<void()> task);

  /** 等待队列和正在执行的任务全部归零，用于规则热更形成确定的会话屏障。 */
  void WaitIdle();

  /**
   * 停止接收新任务并等待已提交任务结束；调用方必须先中断任务持有的阻塞 socket。
   */
  void Stop();

private:
  /** 工作线程循环；任务异常被隔离并交给 failureCallback，不允许越过线程边界。
   */
  void RunWorker() noexcept;

  const std::size_t workerCount_;
  const std::size_t maximumQueuedTasks_;
  const std::function<void()> failureCallback_;
  std::mutex mutex_;
  std::condition_variable taskCondition_;
  std::condition_variable idleCondition_;
  std::deque<std::function<void()>> tasks_;
  std::vector<std::thread> threads_;
  std::size_t executingTasks_ = 0;
  bool running_ = false;
};

} // namespace routesocks::runtime
