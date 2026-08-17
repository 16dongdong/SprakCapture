#include "net/netfilter_queue.h"

#include <arpa/inet.h>
#include <linux/ip.h>
#include <linux/netfilter.h>
#include <linux/netfilter/nfnetlink.h>
#include <linux/netfilter/nfnetlink_queue.h>
#include <linux/netlink.h>
#include <netinet/udp.h>
#include <poll.h>
#include <unistd.h>

#include <array>
#include <cerrno>
#include <cstring>
#include <stdexcept>
#include <utility>

namespace routesocks::net {
namespace {

constexpr std::size_t kMaximumNetlinkMessage = 70 * 1024;
constexpr uint32_t kQueueMaximumLength = 4096;
constexpr int kQueuePollMilliseconds = 200;

/** 把一个 netlink 属性的类型和借用正文绑定为不可分割的写入单元。 */
struct AttributeView {
  uint16_t type;
  const void *value;
  std::size_t length;
};

/** 向 netlink 消息尾部追加一个对齐属性；空间不足返回 false，调用方不得发送截断配置。 */
bool AppendAttribute(nlmsghdr *header, std::size_t capacity,
                     const AttributeView &view) {
  const std::size_t offset = NLMSG_ALIGN(header->nlmsg_len);
  const std::size_t attribute_length = NLA_HDRLEN + view.length;
  const std::size_t aligned_length = NLA_ALIGN(attribute_length);
  if (offset + aligned_length > capacity)
    return false;
  auto *attribute = reinterpret_cast<nlattr *>(
      reinterpret_cast<uint8_t *>(header) + offset);
  attribute->nla_type = view.type;
  attribute->nla_len = static_cast<uint16_t>(attribute_length);
  if (view.length > 0)
    std::memcpy(reinterpret_cast<uint8_t *>(attribute) + NLA_HDRLEN,
                view.value, view.length);
  if (aligned_length > attribute_length) {
    std::memset(reinterpret_cast<uint8_t *>(attribute) + attribute_length, 0,
                aligned_length - attribute_length);
  }
  header->nlmsg_len = static_cast<uint32_t>(offset + aligned_length);
  return true;
}

/** 严格解析 IPv4 UDP 头；损坏长度、分片后续包或零端口不会进入目标映射。 */
bool ParseIpv4Udp(const uint8_t *bytes, std::size_t length,
                  QueuedUdpPacket *packet) {
  if (bytes == nullptr || packet == nullptr || length < sizeof(iphdr))
    return false;
  const auto *ip = reinterpret_cast<const iphdr *>(bytes);
  const std::size_t ip_header_length = static_cast<std::size_t>(ip->ihl) * 4;
  if (ip->version != 4 || ip_header_length < sizeof(iphdr) ||
      length < ip_header_length + sizeof(udphdr) || ip->protocol != IPPROTO_UDP ||
      (ntohs(ip->frag_off) & 0x1FFFU) != 0)
    return false;
  const auto *udp =
      reinterpret_cast<const udphdr *>(bytes + ip_header_length);
  const uint16_t source_port = ntohs(udp->source);
  const uint16_t target_port = ntohs(udp->dest);
  if (source_port == 0 || target_port == 0)
    return false;

  sockaddr_in source{};
  source.sin_family = AF_INET;
  source.sin_addr.s_addr = ip->saddr;
  source.sin_port = udp->source;
  std::memcpy(&packet->source, &source, sizeof(source));
  packet->source_length = sizeof(source);

  char target_address[INET_ADDRSTRLEN]{};
  if (inet_ntop(AF_INET, &ip->daddr, target_address,
                sizeof(target_address)) == nullptr)
    return false;
  packet->target = {target_address, target_port};
  return true;
}

/** 在属性缓冲区中定位指定类型；畸形长度立即停止，避免越界解释内核消息。 */
const nlattr *FindAttribute(const uint8_t *bytes, std::size_t length,
                            uint16_t expected_type) {
  std::size_t offset = 0;
  while (offset + sizeof(nlattr) <= length) {
    const auto *attribute =
        reinterpret_cast<const nlattr *>(bytes + offset);
    if (attribute->nla_len < NLA_HDRLEN ||
        offset + attribute->nla_len > length)
      return nullptr;
    if ((attribute->nla_type & NLA_TYPE_MASK) == expected_type)
      return attribute;
    offset += NLA_ALIGN(attribute->nla_len);
  }
  return nullptr;
}

/** 返回属性正文与长度；属性头损坏时返回空区间。 */
std::pair<const uint8_t *, std::size_t> AttributePayload(
    const nlattr *attribute) {
  if (attribute == nullptr || attribute->nla_len < NLA_HDRLEN)
    return {nullptr, 0};
  return {reinterpret_cast<const uint8_t *>(attribute) + NLA_HDRLEN,
          static_cast<std::size_t>(attribute->nla_len - NLA_HDRLEN)};
}

} // namespace

NetfilterQueue::NetfilterQueue(uint16_t queue_number)
    : queue_number_(queue_number) {}

NetfilterQueue::~NetfilterQueue() { Close(); }

bool NetfilterQueue::Open(std::string *error) {
  if (error == nullptr)
    return false;
  if (descriptor_.load() >= 0) {
    *error = "NFQUEUE 已经打开";
    return false;
  }
  const int descriptor = socket(AF_NETLINK, SOCK_RAW, NETLINK_NETFILTER);
  if (descriptor < 0) {
    *error = "创建 NFQUEUE netlink 失败";
    return false;
  }
  sockaddr_nl address{};
  address.nl_family = AF_NETLINK;
  if (bind(descriptor, reinterpret_cast<const sockaddr *>(&address),
           sizeof(address)) != 0) {
    close(descriptor);
    *error = "绑定 NFQUEUE netlink 失败";
    return false;
  }
  descriptor_.store(descriptor);
  if (!Configure(NFQNL_CFG_CMD_PF_BIND, error) ||
      !Configure(NFQNL_CFG_CMD_BIND, error) || !ConfigureCopy(error)) {
    Close();
    return false;
  }
  return true;
}

bool NetfilterQueue::Configure(uint8_t command, std::string *error) {
  std::array<uint8_t, 256> message{};
  auto *header = reinterpret_cast<nlmsghdr *>(message.data());
  header->nlmsg_len = NLMSG_LENGTH(sizeof(nfgenmsg));
  header->nlmsg_type =
      static_cast<uint16_t>((NFNL_SUBSYS_QUEUE << 8) | NFQNL_MSG_CONFIG);
  header->nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK;
  header->nlmsg_seq = sequence_.fetch_add(1);
  auto *general = reinterpret_cast<nfgenmsg *>(NLMSG_DATA(header));
  general->nfgen_family = AF_INET;
  general->version = NFNETLINK_V0;
  general->res_id = htons(command == NFQNL_CFG_CMD_PF_BIND ? 0 : queue_number_);
  nfqnl_msg_config_cmd configuration{};
  configuration.command = command;
  configuration.pf = htons(AF_INET);
  if (!AppendAttribute(header, message.size(),
                       {NFQA_CFG_CMD, &configuration, sizeof(configuration)})) {
    *error = "构造 NFQUEUE 绑定消息失败";
    return false;
  }
  const int descriptor = descriptor_.load();
  if (send(descriptor, header, header->nlmsg_len, 0) < 0) {
    *error = "发送 NFQUEUE 绑定消息失败";
    return false;
  }
  std::array<uint8_t, 512> response{};
  const ssize_t received = recv(descriptor, response.data(), response.size(), 0);
  if (received < static_cast<ssize_t>(sizeof(nlmsghdr))) {
    *error = "读取 NFQUEUE 绑定结果失败";
    return false;
  }
  const auto *result = reinterpret_cast<const nlmsghdr *>(response.data());
  if (result->nlmsg_type != NLMSG_ERROR ||
      result->nlmsg_len < NLMSG_LENGTH(sizeof(nlmsgerr))) {
    *error = "NFQUEUE 绑定结果格式无效";
    return false;
  }
  const auto *acknowledgement =
      reinterpret_cast<const nlmsgerr *>(NLMSG_DATA(result));
  // PF 绑定是协议族级资源；同一 Root 进程的第二个队列会收到 EBUSY，
  // 但队列号仍可继续独立绑定。其他配置命令绝不忽略该错误。
  if (command == NFQNL_CFG_CMD_PF_BIND &&
      acknowledgement->error == -EBUSY)
    return true;
  if (acknowledgement->error != 0) {
    *error = "NFQUEUE 队列已占用或内核拒绝绑定";
    return false;
  }
  return true;
}

bool NetfilterQueue::ConfigureCopy(std::string *error) {
  std::array<uint8_t, 256> message{};
  auto *header = reinterpret_cast<nlmsghdr *>(message.data());
  header->nlmsg_len = NLMSG_LENGTH(sizeof(nfgenmsg));
  header->nlmsg_type =
      static_cast<uint16_t>((NFNL_SUBSYS_QUEUE << 8) | NFQNL_MSG_CONFIG);
  header->nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK;
  header->nlmsg_seq = sequence_.fetch_add(1);
  auto *general = reinterpret_cast<nfgenmsg *>(NLMSG_DATA(header));
  general->nfgen_family = AF_INET;
  general->version = NFNETLINK_V0;
  general->res_id = htons(queue_number_);
  nfqnl_msg_config_params parameters{};
  parameters.copy_range = htonl(65535);
  parameters.copy_mode = NFQNL_COPY_PACKET;
  const uint32_t maximum_length = htonl(kQueueMaximumLength);
  const uint32_t flags = htonl(NFQA_CFG_F_GSO);
  if (!AppendAttribute(
          header, message.size(),
          {NFQA_CFG_PARAMS, &parameters, sizeof(parameters)}) ||
      !AppendAttribute(
          header, message.size(),
          {NFQA_CFG_QUEUE_MAXLEN, &maximum_length, sizeof(maximum_length)}) ||
      !AppendAttribute(header, message.size(),
                       {NFQA_CFG_FLAGS, &flags, sizeof(flags)}) ||
      !AppendAttribute(header, message.size(),
                       {NFQA_CFG_MASK, &flags, sizeof(flags)})) {
    *error = "构造 NFQUEUE 复制配置失败";
    return false;
  }
  const int descriptor = descriptor_.load();
  if (send(descriptor, header, header->nlmsg_len, 0) < 0) {
    *error = "发送 NFQUEUE 复制配置失败";
    return false;
  }
  std::array<uint8_t, 512> response{};
  const ssize_t received = recv(descriptor, response.data(), response.size(), 0);
  if (received < static_cast<ssize_t>(sizeof(nlmsghdr))) {
    *error = "读取 NFQUEUE 复制配置失败";
    return false;
  }
  const auto *result = reinterpret_cast<const nlmsghdr *>(response.data());
  if (result->nlmsg_type != NLMSG_ERROR ||
      result->nlmsg_len < NLMSG_LENGTH(sizeof(nlmsgerr)) ||
      reinterpret_cast<const nlmsgerr *>(NLMSG_DATA(result))->error != 0) {
    *error = "内核拒绝 NFQUEUE 复制配置";
    return false;
  }
  return true;
}

void NetfilterQueue::Run(const PacketCallback &callback) {
  alignas(nlmsghdr) std::array<uint8_t, kMaximumNetlinkMessage> buffer{};
  while (descriptor_.load() >= 0) {
    const int descriptor = descriptor_.load();
    pollfd event{descriptor, POLLIN, 0};
    const int ready = poll(&event, 1, kQueuePollMilliseconds);
    if (ready < 0) {
      if (errno == EINTR)
        continue;
      return;
    }
    if (ready == 0)
      continue;
    if ((event.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0 ||
        descriptor_.load() != descriptor)
      return;
    if ((event.revents & POLLIN) == 0)
      continue;
    // poll 后使用非阻塞读取，避免 Close 与事件消费交错时再次进入不可唤醒 recv，保证热切换能同步释放队列号。
    const ssize_t received =
        recv(descriptor, buffer.data(), buffer.size(), MSG_DONTWAIT);
    if (received < 0) {
      if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)
        continue;
      return;
    }
    // 内核缓冲区可能在一次 recv 中携带多条消息。必须先复制并固定本条长度，再进入会触发并发停机的回调；
    // 直接使用 NLMSG_NEXT 会在回调后的第二次长度读取中形成 TOCTOU，真机曾因此把 remaining 减到下溢并越界崩溃。
    std::size_t offset = 0;
    const std::size_t total = static_cast<std::size_t>(received);
    while (total - offset >= sizeof(nlmsghdr)) {
      nlmsghdr message_header{};
      std::memcpy(&message_header, buffer.data() + offset,
                  sizeof(message_header));
      const std::size_t message_length = message_header.nlmsg_len;
      const std::size_t remaining = total - offset;
      if (message_length < sizeof(nlmsghdr) || message_length > remaining)
        break;
      ProcessMessage(buffer.data() + offset, message_length, callback);
      const std::size_t aligned_length = NLMSG_ALIGN(message_length);
      if (aligned_length > remaining)
        break;
      offset += aligned_length;
    }
  }
}

void NetfilterQueue::ProcessMessage(const void *bytes, std::size_t length,
                                    const PacketCallback &callback) {
  if (bytes == nullptr || length < NLMSG_LENGTH(sizeof(nfgenmsg)))
    return;
  const auto *header = reinterpret_cast<const nlmsghdr *>(bytes);
  if (header->nlmsg_len < NLMSG_LENGTH(sizeof(nfgenmsg)) ||
      header->nlmsg_len > length)
    return;
  if (header->nlmsg_type !=
      static_cast<uint16_t>((NFNL_SUBSYS_QUEUE << 8) | NFQNL_MSG_PACKET))
    return;
  const auto *attributes = reinterpret_cast<const uint8_t *>(NLMSG_DATA(header)) +
                           NLMSG_ALIGN(sizeof(nfgenmsg));
  const std::size_t attribute_length =
      header->nlmsg_len - NLMSG_LENGTH(sizeof(nfgenmsg));
  const auto packet_header =
      AttributePayload(FindAttribute(attributes, attribute_length,
                                     NFQA_PACKET_HDR));
  const auto payload = AttributePayload(
      FindAttribute(attributes, attribute_length, NFQA_PAYLOAD));
  if (packet_header.second < sizeof(nfqnl_msg_packet_hdr))
    return;
  const auto *metadata =
      reinterpret_cast<const nfqnl_msg_packet_hdr *>(packet_header.first);
  const uint32_t packet_id = ntohl(metadata->packet_id);
  QueuedUdpPacket packet;
  const bool accepted = payload.first != nullptr &&
                        ParseIpv4Udp(payload.first, payload.second, &packet) &&
                        callback(packet);
  // verdict 是内核队列释放数据报的唯一提交点；发送失败后继续运行会让队列静默堆积并表现为随机 UDP 超时。
  if (!SendVerdict(packet_id, accepted))
    throw std::runtime_error("提交 NFQUEUE 数据报裁决失败");
}

bool NetfilterQueue::SendVerdict(uint32_t packet_id, bool accept) {
  std::array<uint8_t, 256> message{};
  auto *header = reinterpret_cast<nlmsghdr *>(message.data());
  header->nlmsg_len = NLMSG_LENGTH(sizeof(nfgenmsg));
  header->nlmsg_type =
      static_cast<uint16_t>((NFNL_SUBSYS_QUEUE << 8) | NFQNL_MSG_VERDICT);
  header->nlmsg_flags = NLM_F_REQUEST;
  header->nlmsg_seq = sequence_.fetch_add(1);
  auto *general = reinterpret_cast<nfgenmsg *>(NLMSG_DATA(header));
  general->nfgen_family = AF_INET;
  general->version = NFNETLINK_V0;
  general->res_id = htons(queue_number_);
  nfqnl_msg_verdict_hdr verdict{};
  verdict.verdict = htonl(accept ? NF_ACCEPT : NF_DROP);
  verdict.id = htonl(packet_id);
  if (!AppendAttribute(header, message.size(),
                       {NFQA_VERDICT_HDR, &verdict, sizeof(verdict)}))
    return false;
  const int descriptor = descriptor_.load();
  return descriptor >= 0 &&
         send(descriptor, header, header->nlmsg_len, 0) >= 0;
}

void NetfilterQueue::Close() noexcept {
  const int descriptor = descriptor_.exchange(-1);
  if (descriptor < 0)
    return;
  shutdown(descriptor, SHUT_RDWR);
  close(descriptor);
}

} // namespace routesocks::net
