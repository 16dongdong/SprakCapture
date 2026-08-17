#pragma once

namespace routesocks::runtime {

/**
 * 请求当前 Android VpnService 把新建 socket 排除出 TUN。
 * 仅 VPN 数据面调用；未注册回调、JNI 异常或系统拒绝都会返回 false，调用方必须在
 * connect/send 前关闭 socket，避免上游连接被自身再次捕获形成递归。
 */
bool ProtectVpnSocket(int descriptor) noexcept;

} // namespace routesocks::runtime
