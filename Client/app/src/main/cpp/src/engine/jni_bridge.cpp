#include <jni.h>

#include <array>
#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <exception>
#include <memory>
#include <mutex>
#include <new>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "core/routing_rules.h"
#include "engine/profile_crypto.h"
#include "engine/proxy_runtime.h"
#include "engine/runtime_config.h"
#include "engine/vpn_socket_protector.h"
#include "hev-flow-classifier.h"
#include "hev-jni.h"

namespace {

constexpr std::size_t kMaximumConfigurationBytes = 4 * 1024;
constexpr std::size_t kMaximumRoutingBytes = 1024 * 1024;

std::mutex runtime_mutex;
std::shared_ptr<routesocks::runtime::ProxyRuntime> active_runtime;
JavaVM *java_vm = nullptr;
jclass classifier_class = nullptr;
jmethodID classifier_method = nullptr;
jclass runtime_class = nullptr;
jmethodID socket_protector_method = nullptr;
constexpr int kDynamicFlowContext = -2;
std::atomic<int> fixed_flow_context{kDynamicFlowContext};
std::mutex classifier_call_mutex;
std::mutex classifier_request_mutex;
std::condition_variable classifier_request_condition;
std::condition_variable classifier_result_condition;
std::thread classifier_thread;
HevFlowTuple classifier_request{};
bool classifier_thread_running = false;
bool classifier_thread_initialized = false;
bool classifier_request_pending = false;
bool classifier_result_ready = false;
int classifier_result = -1;

/**
 * 在普通 Native 工作线程调用 VpnService.protect；当前线程未附着时临时附着并在返回前
 * 解除。Java 异常统一清除并返回失败，socket 尚未 connect，因此不会发生递归流量。
 */
bool InvokeVpnSocketProtector(int descriptor) noexcept {
  if (java_vm == nullptr || runtime_class == nullptr ||
      socket_protector_method == nullptr || descriptor < 0) {
    return false;
  }
  JNIEnv *environment = nullptr;
  bool attached_here = false;
  const jint environment_status = java_vm->GetEnv(
      reinterpret_cast<void **>(&environment), JNI_VERSION_1_6);
  if (environment_status == JNI_EDETACHED) {
    if (java_vm->AttachCurrentThread(&environment, nullptr) != JNI_OK)
      return false;
    attached_here = true;
  } else if (environment_status != JNI_OK) {
    return false;
  }
  const jboolean protected_socket = environment->CallStaticBooleanMethod(
      runtime_class, socket_protector_method, static_cast<jint>(descriptor));
  const bool failed = environment->ExceptionCheck();
  if (failed)
    environment->ExceptionClear();
  if (attached_here)
    java_vm->DetachCurrentThread();
  return !failed && protected_socket == JNI_TRUE;
}

/** 抛出不携带 profile 内容的固定 Java 异常；查找异常类失败时保留 VM 原异常。 */
void ThrowProfileException(JNIEnv *environment, const char *class_name,
                           const char *message) noexcept {
  jclass exception_class = environment->FindClass(class_name);
  if (exception_class == nullptr)
    return;
  environment->ThrowNew(exception_class, message);
  environment->DeleteLocalRef(exception_class);
}

/**
 * 在普通 pthread 上调用 Kotlin UID 归属分类器。
 * HEV 的分类回调运行在协程栈上，直接进入 ART 会越过其栈与线程附着边界；专用线程
 * 只读取全局 JNI 引用，并把任何 Java 异常转换为 REJECT。
 */
int InvokeJavaFlowClassifier(JNIEnv *environment, const HevFlowTuple &flow) {
  std::array<jbyte, 38> bytes{};
  bytes[0] = static_cast<jbyte>(flow.protocol);
  bytes[1] = static_cast<jbyte>(flow.family);
  bytes[2] = static_cast<jbyte>(flow.source_port >> 8U);
  bytes[3] = static_cast<jbyte>(flow.source_port & 0xFFU);
  bytes[4] = static_cast<jbyte>(flow.destination_port >> 8U);
  bytes[5] = static_cast<jbyte>(flow.destination_port & 0xFFU);
  for (std::size_t index = 0; index < 16; ++index) {
    bytes[6 + index] = static_cast<jbyte>(flow.source_address[index]);
    bytes[22 + index] = static_cast<jbyte>(flow.destination_address[index]);
  }
  jbyteArray tuple = environment->NewByteArray(bytes.size());
  if (tuple == nullptr) {
    if (environment->ExceptionCheck())
      environment->ExceptionClear();
    return -1;
  }
  environment->SetByteArrayRegion(tuple, 0, bytes.size(), bytes.data());
  if (environment->ExceptionCheck()) {
    environment->ExceptionClear();
    environment->DeleteLocalRef(tuple);
    return -1;
  }
  const jint result = environment->CallStaticIntMethod(
      classifier_class, classifier_method, tuple);
  environment->DeleteLocalRef(tuple);
  if (environment->ExceptionCheck()) {
    environment->ExceptionClear();
    return -1;
  }
  return result >= -1 && result <= 1 ? result : -1;
}

/**
 * 运行跨协程 JNI 分类执行器；启动握手会报告 AttachCurrentThread 结果。
 * 停止时唤醒所有等待者并返回 REJECT，线程函数不允许异常穿越 pthread 入口。
 */
void RunFlowClassifierThread() noexcept {
  JNIEnv *environment = nullptr;
  const bool attached =
      java_vm != nullptr &&
      java_vm->AttachCurrentThread(&environment, nullptr) == JNI_OK;
  {
    std::lock_guard<std::mutex> lock(classifier_request_mutex);
    classifier_thread_initialized = true;
    classifier_thread_running = attached;
  }
  classifier_result_condition.notify_all();
  if (!attached)
    return;

  while (true) {
    HevFlowTuple flow{};
    {
      std::unique_lock<std::mutex> lock(classifier_request_mutex);
      classifier_request_condition.wait(lock, []() {
        return !classifier_thread_running || classifier_request_pending;
      });
      if (!classifier_thread_running)
        break;
      flow = classifier_request;
      classifier_request_pending = false;
    }

    int result = -1;
    try {
      result = InvokeJavaFlowClassifier(environment, flow);
    } catch (...) {
      result = -1;
    }
    {
      std::lock_guard<std::mutex> lock(classifier_request_mutex);
      classifier_result = result;
      classifier_result_ready = true;
    }
    classifier_result_condition.notify_all();
  }
  java_vm->DetachCurrentThread();
}

/**
 * 确保动态分类器拥有普通 pthread 栈；调用发生在 Native 启动事务内，线程创建或
 * JVM 附着失败会返回 false，HEV 尚未接收 TUN 流量，因此不会留下半启动会话。
 */
bool EnsureFlowClassifierThread(std::string *error) {
  std::unique_lock<std::mutex> lock(classifier_request_mutex);
  if (classifier_thread_running)
    return true;
  classifier_thread_initialized = false;
  try {
    classifier_thread = std::thread(RunFlowClassifierThread);
  } catch (...) {
    *error = "创建 VPN 流分类线程失败";
    return false;
  }
  classifier_result_condition.wait(
      lock, []() { return classifier_thread_initialized; });
  if (classifier_thread_running)
    return true;
  lock.unlock();
  if (classifier_thread.joinable())
    classifier_thread.join();
  *error = "VPN 流分类线程附着 JVM 失败";
  return false;
}

/**
 * 停止分类线程并解除 HEV 回调；用于 SO 卸载边界，所有并发等待统一得到 REJECT，
 * 防止线程继续访问已经释放的 JNI 全局引用。
 */
void StopFlowClassifierThread(JNIEnv *environment) noexcept {
  hev_socks5_tunnel_set_flow_classifier(nullptr, 0);
  {
    std::lock_guard<std::mutex> lock(classifier_request_mutex);
    classifier_thread_running = false;
    classifier_result = -1;
    classifier_result_ready = true;
  }
  classifier_request_condition.notify_all();
  classifier_result_condition.notify_all();
  if (classifier_thread.joinable())
    classifier_thread.join();
  if (classifier_class != nullptr && environment != nullptr) {
    environment->DeleteGlobalRef(classifier_class);
    classifier_class = nullptr;
  }
  classifier_method = nullptr;
}

/**
 * 将 HEV 五元组同步交给专用分类线程。
 * 固定全局/应用规则直接返回缓存上下文；混合规则用串行请求槽避免 HEV 协程栈进入
 * ART，也保证五元组在回调返回前始终有效。停止或线程失效返回 REJECT。
 */
int ClassifyVpnFlow(const HevFlowTuple *flow) noexcept {
  const int fixed_context = fixed_flow_context.load();
  if (fixed_context != kDynamicFlowContext)
    return fixed_context;
  if (flow == nullptr)
    return -1;

  std::lock_guard<std::mutex> call_lock(classifier_call_mutex);
  std::unique_lock<std::mutex> request_lock(classifier_request_mutex);
  if (!classifier_thread_running)
    return -1;
  classifier_request = *flow;
  classifier_result_ready = false;
  classifier_request_pending = true;
  classifier_request_condition.notify_one();
  classifier_result_condition.wait(request_lock, []() {
    return classifier_result_ready || !classifier_thread_running;
  });
  return classifier_thread_running ? classifier_result : -1;
}

/**
 * 在 Native 启动线程探测 Kotlin 当前分类模式。
 * 空五元组在固定模式会直接返回 0/1，混合模式会返回 REJECT；借此让常见固定模式
 * 完全绕开跨线程 JNI，同时不新增 Java ABI。
 * JNI 异常返回动态模式，由专用线程处理。
 */
int DetectFixedFlowContext(JNIEnv *environment) {
  jbyteArray empty_flow = environment->NewByteArray(0);
  if (empty_flow == nullptr) {
    if (environment->ExceptionCheck())
      environment->ExceptionClear();
    return kDynamicFlowContext;
  }
  const jint result = environment->CallStaticIntMethod(
      classifier_class, classifier_method, empty_flow);
  environment->DeleteLocalRef(empty_flow);
  if (environment->ExceptionCheck()) {
    environment->ExceptionClear();
    return kDynamicFlowContext;
  }
  return result == 0 || result == 1 ? result : kDynamicFlowContext;
}

/**
 * 注册静态合入的 HEV 分类回调并同步当前规则上下文。
 * 固定规则只缓存整数；混合规则预先启动 JNI 专用线程，任一初始化失败都会在 HEV
 * 接收 TUN 流量前终止启动事务。
 */
bool RegisterFlowClassifier(JNIEnv *environment, uint16_t selected_port,
                            std::string *error) {
  if (classifier_class == nullptr || classifier_method == nullptr) {
    if (classifier_class != nullptr) {
      environment->DeleteGlobalRef(classifier_class);
      classifier_class = nullptr;
    }
    classifier_method = nullptr;
    jclass local_class =
        environment->FindClass("app/proxy/client/runtime/VpnFlowClassifier");
    if (local_class == nullptr) {
      environment->ExceptionClear();
      *error = "未找到 VPN 五元组分类器";
      return false;
    }
    classifier_class =
        static_cast<jclass>(environment->NewGlobalRef(local_class));
    environment->DeleteLocalRef(local_class);
    if (classifier_class == nullptr) {
      if (environment->ExceptionCheck())
        environment->ExceptionClear();
      *error = "VPN 五元组分类器全局引用创建失败";
      return false;
    }
    classifier_method =
        environment->GetStaticMethodID(classifier_class, "classify", "([B)I");
    if (classifier_method == nullptr) {
      environment->ExceptionClear();
      environment->DeleteGlobalRef(classifier_class);
      classifier_class = nullptr;
      *error = "VPN 五元组分类器签名无效";
      return false;
    }
  }
  const int fixed_context = DetectFixedFlowContext(environment);
  if (fixed_context == kDynamicFlowContext &&
      !EnsureFlowClassifierThread(error)) {
    return false;
  }
  fixed_flow_context.store(fixed_context);
  hev_socks5_tunnel_set_flow_classifier(ClassifyVpnFlow, selected_port);
  return true;
}

/**
 * 缓存 NativeRuntime.protectSocket ABI；VPN 的每个直连、DNS 与上游 SOCKS socket
 * 都必须在 connect 前经过此入口，Root 模式不会调用该回调。
 */
bool RegisterVpnSocketProtector(JNIEnv *environment, std::string *error) {
  if (runtime_class != nullptr && socket_protector_method != nullptr)
    return true;
  jclass local_class =
      environment->FindClass("app/proxy/client/runtime/NativeRuntime");
  if (local_class == nullptr) {
    environment->ExceptionClear();
    *error = "未找到 VPN socket 排除器";
    return false;
  }
  runtime_class = static_cast<jclass>(environment->NewGlobalRef(local_class));
  environment->DeleteLocalRef(local_class);
  if (runtime_class == nullptr) {
    if (environment->ExceptionCheck())
      environment->ExceptionClear();
    *error = "VPN socket 排除器全局引用创建失败";
    return false;
  }
  socket_protector_method = environment->GetStaticMethodID(
      runtime_class, "protectSocket", "(I)Z");
  if (socket_protector_method == nullptr) {
    environment->ExceptionClear();
    environment->DeleteGlobalRef(runtime_class);
    runtime_class = nullptr;
    *error = "VPN socket 排除器签名无效";
    return false;
  }
  return true;
}

struct JavaStringCopyRequest {
  jstring source = nullptr;
  std::size_t maximum_utf8_bytes = 0;
  const char *field_name = nullptr;
  std::string *destination = nullptr;
};

/**
 * 按字段预算把 Java UTF-16 严格编码为标准 UTF-8；先计算精确字节数，再一次性
 * 分配目标缓冲。孤立代理项或超限输入在修改旧运行状态前返回 false，避免 JNI
 * modified UTF-8/CESU-8 漂移及超大配置造成无界分配。
 */
bool CopyJavaString(JNIEnv *environment,
                    const JavaStringCopyRequest &request,
                    std::string *error) {
  if (request.source == nullptr || request.destination == nullptr ||
      request.field_name == nullptr || request.maximum_utf8_bytes == 0) {
    *error = "Native 接口收到无效字符串参数";
    return false;
  }
  const jsize length = environment->GetStringLength(request.source);
  if (static_cast<std::size_t>(length) > request.maximum_utf8_bytes) {
    *error = std::string(request.field_name) + "超过 UTF-8 字节上限";
    return false;
  }
  const jchar *characters =
      environment->GetStringChars(request.source, nullptr);
  if (characters == nullptr) {
    *error = "Native 接口无法读取字符串";
    return false;
  }

  std::size_t encoded_length = 0;
  for (jsize index = 0; index < length; ++index) {
    uint32_t code_point = characters[index];
    if (code_point >= 0xD800 && code_point <= 0xDBFF) {
      if (++index >= length || characters[index] < 0xDC00 ||
          characters[index] > 0xDFFF) {
        environment->ReleaseStringChars(request.source, characters);
        *error = "Native 接口字符串包含孤立高代理项";
        return false;
      }
      code_point = 0x10000 + ((code_point - 0xD800) << 10U) +
                   (characters[index] - 0xDC00);
    } else if (code_point >= 0xDC00 && code_point <= 0xDFFF) {
      environment->ReleaseStringChars(request.source, characters);
      *error = "Native 接口字符串包含孤立低代理项";
      return false;
    }
    encoded_length += code_point <= 0x7F     ? 1
                      : code_point <= 0x7FF  ? 2
                      : code_point <= 0xFFFF ? 3
                                             : 4;
    if (encoded_length > request.maximum_utf8_bytes) {
      environment->ReleaseStringChars(request.source, characters);
      *error = std::string(request.field_name) + "超过 UTF-8 字节上限";
      return false;
    }
  }

  request.destination->clear();
  request.destination->reserve(encoded_length);
  for (jsize index = 0; index < length; ++index) {
    uint32_t code_point = characters[index];
    if (code_point >= 0xD800 && code_point <= 0xDBFF) {
      const uint32_t high = code_point;
      code_point = 0x10000 + ((high - 0xD800) << 10U) +
                   (characters[++index] - 0xDC00);
    }
    if (code_point <= 0x7F) {
      request.destination->push_back(static_cast<char>(code_point));
    } else if (code_point <= 0x7FF) {
      request.destination->push_back(
          static_cast<char>(0xC0U | (code_point >> 6U)));
      request.destination->push_back(
          static_cast<char>(0x80U | (code_point & 0x3FU)));
    } else if (code_point <= 0xFFFF) {
      request.destination->push_back(
          static_cast<char>(0xE0U | (code_point >> 12U)));
      request.destination->push_back(
          static_cast<char>(0x80U | ((code_point >> 6U) & 0x3FU)));
      request.destination->push_back(
          static_cast<char>(0x80U | (code_point & 0x3FU)));
    } else {
      request.destination->push_back(
          static_cast<char>(0xF0U | (code_point >> 18U)));
      request.destination->push_back(
          static_cast<char>(0x80U | ((code_point >> 12U) & 0x3FU)));
      request.destination->push_back(
          static_cast<char>(0x80U | ((code_point >> 6U) & 0x3FU)));
      request.destination->push_back(
          static_cast<char>(0x80U | (code_point & 0x3FU)));
    }
  }
  environment->ReleaseStringChars(request.source, characters);
  return true;
}

/**
 * 严格解码标准 UTF-8 为 Java
 * UTF-16；过长、截断、非最短编码、代理项和越界码点均返回 false。
 * 错误路径也可能拼接用户规则中的补充平面字符，因此不能调用只接收 modified UTF-8
 * 的 NewStringUTF。
 */
bool DecodeUtf8(const std::string &source, std::vector<jchar> *destination) {
  destination->clear();
  for (std::size_t index = 0; index < source.size();) {
    const uint8_t first = static_cast<uint8_t>(source[index++]);
    uint32_t code_point = 0;
    std::size_t continuation_count = 0;
    uint32_t minimum = 0;
    if (first <= 0x7F) {
      code_point = first;
    } else if ((first & 0xE0U) == 0xC0U) {
      code_point = first & 0x1FU;
      continuation_count = 1;
      minimum = 0x80;
    } else if ((first & 0xF0U) == 0xE0U) {
      code_point = first & 0x0FU;
      continuation_count = 2;
      minimum = 0x800;
    } else if ((first & 0xF8U) == 0xF0U) {
      code_point = first & 0x07U;
      continuation_count = 3;
      minimum = 0x10000;
    } else {
      return false;
    }
    if (source.size() - index < continuation_count)
      return false;
    for (std::size_t continuation = 0; continuation < continuation_count;
         ++continuation) {
      const uint8_t byte = static_cast<uint8_t>(source[index++]);
      if ((byte & 0xC0U) != 0x80U)
        return false;
      code_point = (code_point << 6U) | (byte & 0x3FU);
    }
    if (code_point < minimum || code_point > 0x10FFFF ||
        (code_point >= 0xD800 && code_point <= 0xDFFF))
      return false;
    if (code_point <= 0xFFFF) {
      destination->push_back(static_cast<jchar>(code_point));
    } else {
      code_point -= 0x10000;
      destination->push_back(static_cast<jchar>(0xD800U + (code_point >> 10U)));
      destination->push_back(
          static_cast<jchar>(0xDC00U + (code_point & 0x3FFU)));
    }
  }
  return true;
}

/** 将 null 表示成功、标准 UTF-8 中文字符串表示失败的契约统一转换为 JNI 返回值。
 */
jstring ResultString(JNIEnv *environment, const std::string &error) {
  if (error.empty())
    return nullptr;
  std::vector<jchar> utf16;
  if (!DecodeUtf8(error, &utf16)) {
    constexpr std::array<jchar, 13> invalid{{'N', 'a', 't', 'i', 'v', 'e', ' ',
                                             0x9519, 0x8BEF, 0x7F16, 0x7801,
                                             0x5F02, 0x5E38}};
    return environment->NewString(invalid.data(), invalid.size());
  }
  return environment->NewString(utf16.data(), static_cast<jsize>(utf16.size()));
}

/**
 * 在 C++ 已无法安全分配容器时直接构造固定 Java 字符串。
 * 该路径不创建 std::string/vector，保证 bad_alloc 边界不会因二次分配再次抛出
 * C++ 异常。
 */
jstring FixedNativeFailure(JNIEnv *environment,
                           bool resource_exhausted) noexcept {
  constexpr std::array<jchar, 11> resource_error{
      {'N', 'a', 't', 'i', 'v', 'e', ' ', 0x8D44, 0x6E90, 0x4E0D, 0x8DB3}};
  constexpr std::array<jchar, 11> internal_error{
      {'N', 'a', 't', 'i', 'v', 'e', ' ', 0x5185, 0x90E8, 0x5F02, 0x5E38}};
  const auto &message = resource_exhausted ? resource_error : internal_error;
  return environment->NewString(message.data(),
                                static_cast<jsize>(message.size()));
}

/** 构造固定监听线程错误，供 Kotlin 采样在无 std 分配下识别数据面致命失效。 */
jstring FixedListenerFailure(JNIEnv *environment) noexcept {
  constexpr std::array<jchar, 13> message{{'N', 'a', 't', 'i', 'v', 'e', ' ',
                                           0x76D1, 0x542C, 0x7EBF, 0x7A0B,
                                           0x5F02, 0x5E38}};
  return environment->NewString(message.data(),
                                static_cast<jsize>(message.size()));
}

/**
 * 统一收口所有返回错误字符串的 JNI 入口，任何 C++ 异常都转为固定中文结果。
 * operation 必须自行保持业务事务性；本边界只负责阻止异常穿越 extern "C" JNI
 * ABI。
 */
template <typename Operation>
jstring InvokeStringBoundary(JNIEnv *environment,
                             Operation &&operation) noexcept {
  try {
    return operation();
  } catch (const std::bad_alloc &) {
    return FixedNativeFailure(environment, true);
  } catch (const std::exception &) {
    return FixedNativeFailure(environment, false);
  } catch (...) {
    return FixedNativeFailure(environment, false);
  }
}

} // namespace

/**
 * 认证解密 profile.bin 并返回已严格校验的二进制字段序列。
 * Java 数组创建或复制结束后立即擦除 Native
 * 明文；认证与格式失败抛固定异常且不返回部分内容。
 */
extern "C" JNIEXPORT jbyteArray JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeDecryptProfile(
    JNIEnv *environment, jclass, jbyteArray encrypted_profile) {
  std::vector<uint8_t> plaintext;
  try {
    if (encrypted_profile == nullptr) {
      ThrowProfileException(environment, "java/lang/IllegalArgumentException",
                            "节点配置密文为空");
      return nullptr;
    }
    const jsize encrypted_size = environment->GetArrayLength(encrypted_profile);
    std::vector<uint8_t> container(static_cast<std::size_t>(encrypted_size));
    if (encrypted_size > 0) {
      environment->GetByteArrayRegion(
          encrypted_profile, 0, encrypted_size,
          reinterpret_cast<jbyte *>(container.data()));
      if (environment->ExceptionCheck())
        return nullptr;
    }
    std::string error;
    if (!routesocks::runtime::DecryptProfile(container.data(), container.size(),
                                             &plaintext, &error)) {
      ThrowProfileException(environment, "java/lang/IllegalArgumentException",
                            "节点配置认证或格式校验失败");
      return nullptr;
    }
    jbyteArray output =
        environment->NewByteArray(static_cast<jsize>(plaintext.size()));
    if (output != nullptr) {
      environment->SetByteArrayRegion(
          output, 0, static_cast<jsize>(plaintext.size()),
          reinterpret_cast<const jbyte *>(plaintext.data()));
    }
    routesocks::runtime::WipeProfile(&plaintext);
    return environment->ExceptionCheck() ? nullptr : output;
  } catch (const std::bad_alloc &) {
    routesocks::runtime::WipeProfile(&plaintext);
    ThrowProfileException(environment, "java/lang/OutOfMemoryError",
                          "Native 节点配置内存不足");
    return nullptr;
  } catch (...) {
    routesocks::runtime::WipeProfile(&plaintext);
    ThrowProfileException(environment, "java/lang/IllegalStateException",
                          "Native 节点配置内部异常");
    return nullptr;
  }
}

/** 启动统一数据面；null 表示成功，字符串表示可直接展示的启动原因。 */
extern "C" JNIEXPORT jstring JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeStart(
    JNIEnv *environment, jclass, jstring configuration_text,
    jstring routing_text, jboolean root_mode) {
  return InvokeStringBoundary(environment, [&]() -> jstring {
    std::string configuration;
    std::string routing;
    std::string error;
    if (!CopyJavaString(environment,
                        {configuration_text, kMaximumConfigurationBytes,
                         "Native 配置", &configuration},
                        &error) ||
        !CopyJavaString(
            environment,
            {routing_text, kMaximumRoutingBytes, "规则正文", &routing},
            &error)) {
      return ResultString(environment, error);
    }
    routesocks::runtime::RuntimeConfig parsed_config;
    routesocks::core::RoutingRules parsed_rules;
    if (!routesocks::runtime::ParseRuntimeConfig(configuration, routing,
                                                 &parsed_config, &error) ||
        !routesocks::core::RoutingRules::ParseFromText(routing, &parsed_rules,
                                                       &error)) {
      return ResultString(environment, error);
    }
    // 分类回调和端口属于进程级 HEV 状态，必须与 runtime 的唯一实例检查处于同一
    // 临界区；否则并发或重复启动会在返回“已经启动”前篡改正在服务的规则上下文。
    std::lock_guard<std::mutex> lock(runtime_mutex);
    if (active_runtime != nullptr)
      return ResultString(environment, "Native 数据面已经启动");
    if ((root_mode != JNI_TRUE &&
         !RegisterVpnSocketProtector(environment, &error)) ||
        !RegisterFlowClassifier(environment, parsed_config.selected_socks_port,
                                &error)) {
      return ResultString(environment, error);
    }
    auto runtime = std::make_shared<routesocks::runtime::ProxyRuntime>(
        std::move(parsed_config), std::move(parsed_rules),
        root_mode == JNI_TRUE);
    if (!runtime->Start(&error))
      return ResultString(environment, error);
    active_runtime = std::move(runtime);
    return nullptr;
  });
}

/** 幂等停止当前 Native 实例；未启动时不产生副作用。 */
extern "C" JNIEXPORT void JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeStop(JNIEnv *, jclass) {
  std::shared_ptr<routesocks::runtime::ProxyRuntime> runtime;
  try {
    {
      std::lock_guard<std::mutex> lock(runtime_mutex);
      runtime = std::move(active_runtime);
    }
    if (runtime != nullptr)
      runtime->Stop();
  } catch (...) {
    // Stop 异常时把实例所有权送回全局，避免局部 shared_ptr
    // 析构再次进入同一失败清理路径；C++ 异常也不会穿越 void JNI ABI。
    try {
      std::lock_guard<std::mutex> lock(runtime_mutex);
      if (active_runtime == nullptr)
        active_runtime = std::move(runtime);
    } catch (...) {
      // mutex 本身已失效时不存在可靠的全局状态写回通道，只保证异常不外溢。
    }
  }
}

/** 校验并热替换路由和 DNS，成功前不改变现有运行配置。 */
extern "C" JNIEXPORT jstring JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeUpdateRules(
    JNIEnv *environment, jclass, jstring routing_text) {
  return InvokeStringBoundary(environment, [&]() -> jstring {
    std::string routing;
    std::string error;
    if (!CopyJavaString(environment,
                        {routing_text, kMaximumRoutingBytes, "规则正文",
                         &routing},
                        &error)) {
      return ResultString(environment, error);
    }
    routesocks::core::RoutingRules parsed_rules;
    std::vector<routesocks::runtime::Endpoint> dns_servers;
    if (!routesocks::core::RoutingRules::ParseFromText(routing, &parsed_rules,
                                                       &error)) {
      return ResultString(environment, error);
    }
    if (!routesocks::runtime::ParseDnsServers(routing, &dns_servers, &error)) {
      return ResultString(environment, error);
    }
    std::shared_ptr<routesocks::runtime::ProxyRuntime> runtime;
    {
      std::lock_guard<std::mutex> lock(runtime_mutex);
      runtime = active_runtime;
    }
    if (runtime == nullptr)
      return ResultString(environment, "Native 数据面尚未启动");
    runtime->UpdateRules(std::move(parsed_rules), std::move(dns_servers));
    return nullptr;
  });
}

/** 返回固定五字段单调统计；未启动时返回全零而不是空数组。 */
extern "C" JNIEXPORT jlongArray JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeStats(JNIEnv *environment,
                                                        jclass) {
  try {
    std::shared_ptr<routesocks::runtime::ProxyRuntime> runtime;
    {
      std::lock_guard<std::mutex> lock(runtime_mutex);
      runtime = active_runtime;
    }
    const routesocks::runtime::RuntimeStats stats =
        runtime == nullptr ? routesocks::runtime::RuntimeStats{}
                           : runtime->Stats();
    const std::array<jlong, 5> values{
        {static_cast<jlong>(stats.upload_bytes),
         static_cast<jlong>(stats.download_bytes),
         static_cast<jlong>(stats.active_connections),
         static_cast<jlong>(stats.accepted_connections),
         static_cast<jlong>(stats.failed_connections)}};
    jlongArray output = environment->NewLongArray(values.size());
    if (output != nullptr) {
      environment->SetLongArrayRegion(output, 0, values.size(), values.data());
    }
    return output;
  } catch (...) {
    constexpr std::array<jlong, 5> empty{};
    jlongArray output = environment->NewLongArray(empty.size());
    if (output != nullptr)
      environment->SetLongArrayRegion(output, 0, empty.size(), empty.data());
    return output;
  }
}

/** 返回监听线程致命错误；null 表示当前 runtime 健康或尚未启动。 */
extern "C" JNIEXPORT jstring JNICALL
Java_app_proxy_client_runtime_NativeRuntime_nativeHealth(JNIEnv *environment,
                                                         jclass) {
  try {
    std::shared_ptr<routesocks::runtime::ProxyRuntime> runtime;
    {
      std::lock_guard<std::mutex> lock(runtime_mutex);
      runtime = active_runtime;
    }
    return runtime == nullptr || runtime->Healthy()
               ? nullptr
               : FixedListenerFailure(environment);
  } catch (...) {
    return FixedNativeFailure(environment, false);
  }
}

/** 初始化唯一业务 SO 的两组 JNI 接口；任一注册失败都阻止库进入半可用状态。 */
extern "C" JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *virtual_machine, void *) {
  try {
    java_vm = virtual_machine;
    if (hev_jni_initialize(virtual_machine) == JNI_ERR) {
      java_vm = nullptr;
      return JNI_ERR;
    }
    return JNI_VERSION_1_6;
  } catch (...) {
    java_vm = nullptr;
    return JNI_ERR;
  }
}

/**
 * 卸载唯一业务 SO 前停止分类线程并释放 JNI 全局引用。
 * Android 通常以进程退出结束本库，本边界仍保证测试宿主主动卸载时不会因 joinable
 * std::thread 触发 terminate。
 * 无法取得 JNIEnv 时线程仍会停止，引用交由 VM 回收。
 */
extern "C" JNIEXPORT void JNICALL JNI_OnUnload(JavaVM *virtual_machine,
                                               void *) {
  JNIEnv *environment = nullptr;
  if (virtual_machine != nullptr) {
    virtual_machine->GetEnv(reinterpret_cast<void **>(&environment),
                            JNI_VERSION_1_6);
  }
  StopFlowClassifierThread(environment);
  if (runtime_class != nullptr && environment != nullptr) {
    environment->DeleteGlobalRef(runtime_class);
    runtime_class = nullptr;
  }
  socket_protector_method = nullptr;
  java_vm = nullptr;
}

namespace routesocks::runtime {

/** 将数据面 socket 排除请求转交已缓存的 Java ABI；异常边界由桥接函数统一处理。 */
bool ProtectVpnSocket(int descriptor) noexcept {
  return InvokeVpnSocketProtector(descriptor);
}

} // namespace routesocks::runtime
