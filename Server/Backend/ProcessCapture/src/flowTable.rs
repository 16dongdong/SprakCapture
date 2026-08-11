use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, RwLock, mpsc::SyncSender},
    time::{Duration, Instant},
};

use crate::normalizeIpAddress;

pub(crate) const tcpProtocol: u8 = 6;
pub(crate) const udpProtocol: u8 = 17;
const firstReflectedPort: u16 = 49_152;
const fragmentDecisionLifetime: Duration = Duration::from_secs(30);
const maximumFragmentDecisions: usize = 4_096;

/// 表示 SOCKET/FLOW 层观测到的原始 TCP 或 UDP 五元组。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CaptureFlow {
    pub processId: u32,
    pub endpointId: u64,
    pub localAddress: IpAddr,
    pub localPort: u16,
    pub remoteAddress: IpAddr,
    pub remotePort: u16,
    pub protocol: u8,
}

/// 汇聚一次 UDP 首包关联所需的五元组、代理入口与捕获水位，避免调用链传递易错的平行参数。
pub(crate) struct UdpAssociationRequest {
    pub localAddress: IpAddr,
    pub localPort: u16,
    pub remoteAddress: IpAddr,
    pub remotePort: u16,
    pub configuredProxyAddress: IpAddr,
    pub proxyPort: u16,
    pub capturedAtCounter: Option<i64>,
}

impl CaptureFlow {
    /// 创建规范化的 TCP 流；非 TCP、零端口或回环目标不会进入改写表。
    pub fn tcp(
        processId: u32,
        endpointId: u64,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
    ) -> Option<Self> {
        Self::transport(
            processId,
            endpointId,
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
            tcpProtocol,
        )
    }

    /// 创建规范化的 UDP 流；FLOW 层为无连接 `sendto` 提供远端端点，供 NETWORK 层精确归属数据报。
    pub fn udp(
        processId: u32,
        endpointId: u64,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
    ) -> Option<Self> {
        Self::transport(
            processId,
            endpointId,
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
            udpProtocol,
        )
    }

    /// 统一校验并构造传输层五元组；回环流量不属于 WinDivert 进程捕获边界。
    fn transport(
        processId: u32,
        endpointId: u64,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
        protocol: u8,
    ) -> Option<Self> {
        let localAddress = normalizeIpAddress(localAddress);
        let remoteAddress = normalizeIpAddress(remoteAddress);
        if localPort == 0
            || remotePort == 0
            || remoteAddress.is_loopback()
            || localAddress.is_ipv4() != remoteAddress.is_ipv4()
        {
            return None;
        }
        Some(Self {
            processId,
            endpointId,
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
            protocol,
        })
    }
}

/// 透明连接需要恢复的原目标；监听器以反射连接的 peer 地址和源端口查询。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginalTarget {
    pub processId: u32,
    pub address: SocketAddr,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct OutboundKey {
    localAddress: IpAddr,
    localPort: u16,
    remoteAddress: IpAddr,
    remotePort: u16,
    protocol: u8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ReflectedKey {
    localAddress: IpAddr,
    localPort: u16,
    remoteAddress: IpAddr,
    protocol: u8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct UdpBindingKey {
    localAddress: IpAddr,
    localPort: u16,
}

/// 精确标识一个 IP 分片组；方向字段避免未来扩展到入站捕获时复用错误决策。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct FragmentKey {
    pub sourceAddress: IpAddr,
    pub destinationAddress: IpAddr,
    pub identification: u32,
    pub protocol: u8,
    pub outbound: bool,
}

/// 描述首片完整五元组生成的后续片动作；Allow 不携带端点，Block 随端点关闭同步清理。
#[derive(Clone, Copy)]
pub(crate) enum FragmentAction {
    Allow,
    Block {
        endpointId: u64,
        target: OriginalTarget,
    },
}

/// 返回首片决策登记结果，使调用方显式处理端点已经关闭的竞态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentRecordResult {
    Recorded,
    EndpointGone,
}

/// 返回后续片的精确决策；未知键始终旁路，避免容量压力跨流影响未选进程。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentLookup {
    Allow,
    Block(OriginalTarget),
    Unknown,
}

#[derive(Clone, Copy)]
/// 保存有界分片决策的动作和绝对过期时刻，防止 IP identification 复用旧状态。
struct FragmentDecision {
    action: FragmentAction,
    expiresAt: Instant,
}

struct FlowState {
    outbound: HashMap<OutboundKey, TrackedTarget>,
    retiredUdpOutbound: HashMap<OutboundKey, Vec<RetiredUdpTarget>>,
    reflected: HashMap<ReflectedKey, RewriteTarget>,
    endpointKeys: HashMap<u64, Vec<(OutboundKey, ReflectedKey)>>,
    udpBindings: HashMap<UdpBindingKey, HashMap<u64, u32>>,
    udpBindingKeys: HashMap<u64, UdpBindingKey>,
    fragmentDecisions: HashMap<FragmentKey, FragmentDecision>,
    nextReflectedPort: u16,
}

impl Default for FlowState {
    /// 从动态端口区顺序分配反射源端口，保证同本地端口到同远端 IP 的并发流仍有唯一 TCP 元组。
    fn default() -> Self {
        Self {
            outbound: HashMap::new(),
            retiredUdpOutbound: HashMap::new(),
            reflected: HashMap::new(),
            endpointKeys: HashMap::new(),
            udpBindings: HashMap::new(),
            udpBindingKeys: HashMap::new(),
            fragmentDecisions: HashMap::new(),
            nextReflectedPort: firstReflectedPort,
        }
    }
}

#[derive(Clone, Copy)]
struct TrackedTarget {
    endpointId: u64,
    rewrite: RewriteTarget,
    activeFromCounter: Option<i64>,
}

/// 保留端点关闭前已经进入拦截队列的 UDP 五元组；捕获时刻晚于关闭时刻的复用端口不得命中。
#[derive(Clone, Copy)]
struct RetiredUdpTarget {
    rewrite: RewriteTarget,
    activeFromCounter: Option<i64>,
    retiredAt: Instant,
    retiredAtCounter: Option<i64>,
}

#[derive(Clone, Copy)]
pub(crate) struct RewriteTarget {
    pub endpointId: u64,
    pub original: OriginalTarget,
    pub originalLocalAddress: IpAddr,
    pub proxyAddress: IpAddr,
    pub originalLocalPort: u16,
    pub reflectedPort: u16,
    pub originalInterface: Option<NetworkInterface>,
}

/// 保存原始出站包的网络接口，供代理回复恢复为入站包时重新选择正确的外部接口。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkInterface {
    pub interfaceIndex: u32,
    pub subinterfaceIndex: u32,
}

/// 以一次写锁原子维护三张索引，避免 CONNECT、CLOSE 与数据包线程观察到半更新状态。
#[derive(Clone, Default)]
pub struct CaptureFlowTable {
    state: Arc<RwLock<FlowState>>,
    associationNotifier: Arc<Mutex<Option<SyncSender<()>>>>,
}

impl CaptureFlowTable {
    /// 登记目标进程的 CONNECT/ESTABLISHED 事件；`configuredProxyAddress` 是融合监听地址。
    /// TCP 端点只保留一条连接；UDP 端点可面向多个远端并共享生命周期。重复五元组、地址族不兼容
    /// 或端口耗尽时返回 `false`，调用方据此避免重复累计已接受数量。
    pub fn insert(
        &self,
        flow: CaptureFlow,
        configuredProxyAddress: IpAddr,
        proxyPort: u16,
    ) -> bool {
        self.insertAt(flow, configuredProxyAddress, proxyPort, None)
    }

    /// 按 WinDivert QPC 事件时刻登记流版本；UDP resolver 用同一时钟选择端口复用前后的正确进程。
    pub(crate) fn insertAt(
        &self,
        flow: CaptureFlow,
        configuredProxyAddress: IpAddr,
        proxyPort: u16,
        activeFromCounter: Option<i64>,
    ) -> bool {
        let Some(proxyAddress) = resolveProxyAddress(flow.localAddress, configuredProxyAddress)
        else {
            return false;
        };
        let outboundKey = OutboundKey {
            localAddress: flow.localAddress,
            localPort: flow.localPort,
            remoteAddress: flow.remoteAddress,
            remotePort: flow.remotePort,
            protocol: flow.protocol,
        };
        let target = OriginalTarget {
            processId: flow.processId,
            address: SocketAddr::new(flow.remoteAddress, flow.remotePort),
        };
        let mut state = self.state.write().expect("流表写锁中毒");
        if flow.protocol == tcpProtocol {
            removeEndpointLocked(&mut state, flow.endpointId, None);
        }
        if let Some(previous) = state.outbound.get(&outboundKey).copied() {
            if previous.endpointId == flow.endpointId {
                return false;
            }
            removeEndpointLocked(
                &mut state,
                previous.endpointId,
                (flow.protocol == udpProtocol)
                    .then_some(activeFromCounter)
                    .flatten(),
            );
        }
        let Some(reflectedPort) = allocateReflectedPort(
            &mut state,
            proxyAddress,
            proxyAddress,
            proxyPort,
            flow.protocol,
        ) else {
            return false;
        };
        let reflectedKey = ReflectedKey {
            localAddress: proxyAddress,
            localPort: reflectedPort,
            remoteAddress: proxyAddress,
            protocol: flow.protocol,
        };
        let rewrite = RewriteTarget {
            endpointId: flow.endpointId,
            original: target,
            originalLocalAddress: flow.localAddress,
            proxyAddress,
            originalLocalPort: flow.localPort,
            reflectedPort,
            originalInterface: None,
        };
        state.outbound.insert(
            outboundKey,
            TrackedTarget {
                endpointId: flow.endpointId,
                rewrite,
                activeFromCounter,
            },
        );
        state.reflected.insert(reflectedKey, rewrite);
        state
            .endpointKeys
            .entry(flow.endpointId)
            .or_default()
            .push((outboundKey, reflectedKey));
        drop(state);
        if let Some(notifier) = self
            .associationNotifier
            .lock()
            .expect("关联通知锁中毒")
            .as_ref()
        {
            let _ = notifier.try_send(());
        }
        true
    }

    /// 安装有界解析器唤醒通道；仅发送无状态通知，连接身份始终由精确流表查询决定。
    pub(crate) fn setAssociationNotifier(&self, notifier: Option<SyncSender<()>>) {
        *self.associationNotifier.lock().expect("关联通知锁中毒") = notifier;
    }

    /// 按 SOCKET/FLOW 的端点编号删除全部索引；未知端点表示事件已被前序线程处理。
    pub fn removeEndpoint(&self, endpointId: u64) {
        let mut state = self.state.write().expect("流表写锁中毒");
        removeEndpointLocked(&mut state, endpointId, None);
    }

    /// 按 WinDivert 的 QPC 事件时刻移除端点；拦截队列中更早的 UDP 包仍可完成归属。
    pub(crate) fn removeEndpointsAt(&self, endpointIds: &[u64], eventTimestamp: i64) {
        let mut state = self.state.write().expect("流表写锁中毒");
        for endpointId in endpointIds {
            removeEndpointLocked(&mut state, *endpointId, Some(eventTimestamp));
        }
    }

    /// 登记选中进程的 UDP BIND 端点；无连接 `sendto` 在首包前只有本地端点可用于 PID 归属。
    /// 同一端点重复绑定会原子替换旧键，零端口和回环绑定不进入外部流量观察表。
    pub(crate) fn registerUdpBinding(
        &self,
        processId: u32,
        endpointId: u64,
        localAddress: IpAddr,
        localPort: u16,
    ) -> bool {
        let localAddress = normalizeIpAddress(localAddress);
        if localPort == 0 || localAddress.is_loopback() {
            return false;
        }
        let key = UdpBindingKey {
            localAddress,
            localPort,
        };
        let mut state = self.state.write().expect("流表写锁中毒");
        removeUdpBindingLocked(&mut state, endpointId);
        state
            .udpBindings
            .entry(key)
            .or_default()
            .insert(endpointId, processId);
        state.udpBindingKeys.insert(endpointId, key);
        true
    }

    /// 在单一写锁内差量替换 owner 表绑定；未变化端点及其既有远端映射保持连续可见。
    pub(crate) fn replaceUdpOwnerBindings(
        &self,
        removedEndpointIds: &[u64],
        additions: &[(u32, u64, IpAddr, u16)],
    ) {
        let mut state = self.state.write().expect("流表写锁中毒");
        for endpointId in removedEndpointIds {
            removeEndpointLocked(&mut state, *endpointId, None);
        }
        for (processId, endpointId, localAddress, localPort) in additions {
            let localAddress = normalizeIpAddress(*localAddress);
            if *localPort == 0 || localAddress.is_loopback() {
                continue;
            }
            removeUdpBindingLocked(&mut state, *endpointId);
            let key = UdpBindingKey {
                localAddress,
                localPort: *localPort,
            };
            state
                .udpBindings
                .entry(key)
                .or_default()
                .insert(*endpointId, *processId);
            state.udpBindingKeys.insert(*endpointId, key);
        }
    }

    /// 以 UDP BIND 的 PID 归属补建首个出站数据报五元组；共享端口存在歧义时保持旁路。
    ///
    /// 运行上下文：NETWORK 可能先于 FLOW ESTABLISHED 看见首包；本函数只在精确或同地址族通配
    /// 本地端点唯一归属一个选中进程时建表。CLOSE 竞态会在插入后复查绑定并立即清除陈旧索引。
    #[cfg(test)]
    pub(crate) fn associateUdpOutbound(
        &self,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
        configuredProxyAddress: IpAddr,
        proxyPort: u16,
    ) -> Option<RewriteTarget> {
        self.associateUdpOutboundAt(UdpAssociationRequest {
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
            configuredProxyAddress,
            proxyPort,
            capturedAtCounter: None,
        })
    }

    /// 以当前拦截包的 QPC 时刻补建 UDP 五元组，使后续端口复用仍可按捕获代际查询。
    pub(crate) fn associateUdpOutboundAt(
        &self,
        request: UdpAssociationRequest,
    ) -> Option<RewriteTarget> {
        let UdpAssociationRequest {
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
            configuredProxyAddress,
            proxyPort,
            capturedAtCounter,
        } = request;
        let existing = capturedAtCounter.map_or_else(
            || {
                self.outboundTransportTarget(
                    udpProtocol,
                    localAddress,
                    localPort,
                    remoteAddress,
                    remotePort,
                )
            },
            |capturedAtCounter| {
                self.udpTargetAt(
                    localAddress,
                    localPort,
                    remoteAddress,
                    remotePort,
                    Instant::now(),
                    capturedAtCounter,
                )
            },
        );
        if let Some(target) = existing {
            return Some(target);
        }
        let localAddress = normalizeIpAddress(localAddress);
        let remoteAddress = normalizeIpAddress(remoteAddress);
        let wildcardAddress = if localAddress.is_ipv4() {
            IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
        };
        let candidate = {
            let state = self.state.read().expect("流表读锁中毒");
            let mut candidates = [
                state.udpBindings.get(&UdpBindingKey {
                    localAddress,
                    localPort,
                }),
                state.udpBindings.get(&UdpBindingKey {
                    localAddress: wildcardAddress,
                    localPort,
                }),
            ]
            .into_iter()
            .flatten()
            .flat_map(|bindings| {
                bindings
                    .iter()
                    .map(|(endpointId, processId)| (*endpointId, *processId))
            })
            .collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates.dedup();
            let uniqueProcessId = candidates.first().map(|(_, processId)| *processId);
            if uniqueProcessId.is_some()
                && candidates
                    .iter()
                    .all(|(_, processId)| Some(*processId) == uniqueProcessId)
            {
                // owner 表预注册和后续 SOCKET BIND 可能同时描述同一进程端点；PID 唯一即可
                // 安全归属，并优先较小的真实 endpointId，禁止把同进程重复行误判为跨进程歧义。
                Some(candidates[0])
            } else {
                None
            }
        }?;
        let (endpointId, processId) = candidate;
        let flow = CaptureFlow::udp(
            processId,
            endpointId,
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
        )?;
        let _ = self.insertAt(flow, configuredProxyAddress, proxyPort, capturedAtCounter);
        let target = capturedAtCounter.map_or_else(
            || {
                self.outboundTransportTarget(
                    udpProtocol,
                    localAddress,
                    localPort,
                    remoteAddress,
                    remotePort,
                )
            },
            |capturedAtCounter| {
                self.udpTargetAt(
                    localAddress,
                    localPort,
                    remoteAddress,
                    remotePort,
                    Instant::now(),
                    capturedAtCounter,
                )
            },
        );
        let bindingStillActive = self
            .state
            .read()
            .expect("流表读锁中毒")
            .udpBindingKeys
            .contains_key(&endpointId);
        if !bindingStillActive {
            self.removeEndpoint(endpointId);
        }
        target
    }

    /// 仅保留当前仍被选择进程拥有的流，并以一次写锁同步移除出站、反射和分片索引。
    ///
    /// 运行上下文：路径监视器原子替换 PID 集后调用本函数；移除中的旧连接随后被系统强制关闭，
    /// 因此这里不得等待 CLOSE 事件，也不得让已取消选择的端点继续命中透明监听器。
    pub fn retainProcessIds(&self, selectedProcessIds: &BTreeSet<u32>) {
        let mut state = self.state.write().expect("流表写锁中毒");
        let removedEndpointIds = state
            .outbound
            .values()
            .filter(|target| !selectedProcessIds.contains(&target.rewrite.original.processId))
            .map(|target| target.endpointId)
            .collect::<Vec<_>>();
        for endpointId in removedEndpointIds {
            removeEndpointLocked(&mut state, endpointId, None);
        }
        let removedBindingEndpointIds = state
            .udpBindings
            .values()
            .flat_map(|bindings| bindings.iter())
            .filter(|(_, processId)| !selectedProcessIds.contains(processId))
            .map(|(endpointId, _)| *endpointId)
            .collect::<Vec<_>>();
        for endpointId in removedBindingEndpointIds {
            removeUdpBindingLocked(&mut state, endpointId);
        }
    }

    /// 记录首片按完整五元组得出的动作；容量已满时确定性淘汰最早过期的旧组。
    pub(crate) fn recordFragmentDecision(
        &self,
        key: FragmentKey,
        action: FragmentAction,
    ) -> FragmentRecordResult {
        self.recordFragmentDecisionAt(key, action, Instant::now())
    }

    /// 查询非首片的精确动作；未知键始终旁路，淘汰旧组不会影响其它进程。
    pub(crate) fn fragmentLookup(&self, key: FragmentKey) -> FragmentLookup {
        self.fragmentLookupAt(key, Instant::now())
    }

    /// 使用显式时钟写入决策，供生产包装和无等待的过期测试共享同一实现。
    fn recordFragmentDecisionAt(
        &self,
        key: FragmentKey,
        action: FragmentAction,
        now: Instant,
    ) -> FragmentRecordResult {
        let mut state = self.state.write().expect("流表写锁中毒");
        pruneFragmentState(&mut state, now);
        if let FragmentAction::Block { endpointId, .. } = action
            && !state.endpointKeys.contains_key(&endpointId)
        {
            return FragmentRecordResult::EndpointGone;
        }
        if matches!(action, FragmentAction::Allow)
            && matches!(
                state.fragmentDecisions.get(&key).map(|entry| entry.action),
                Some(FragmentAction::Block { .. })
            )
        {
            return FragmentRecordResult::Recorded;
        }
        if state.fragmentDecisions.len() >= maximumFragmentDecisions
            && !state.fragmentDecisions.contains_key(&key)
            && let Some(evictionKey) = state
                .fragmentDecisions
                .iter()
                .min_by_key(|(existingKey, decision)| (decision.expiresAt, **existingKey))
                .map(|(existingKey, _)| *existingKey)
        {
            // 首片已在当前调用中按精确五元组决定去留；淘汰旧组只会让其孤立后续片旁路，
            // 缺少首片的远端无法重组 TCP 数据，同时不会用全局状态误伤其它进程。
            state.fragmentDecisions.remove(&evictionKey);
        }
        state.fragmentDecisions.insert(
            key,
            FragmentDecision {
                action,
                expiresAt: now + fragmentDecisionLifetime,
            },
        );
        FragmentRecordResult::Recorded
    }

    /// 使用显式时钟查询决策，保证测试可确定性覆盖过期边界。
    fn fragmentLookupAt(&self, key: FragmentKey, now: Instant) -> FragmentLookup {
        let mut state = self.state.write().expect("流表写锁中毒");
        pruneFragmentState(&mut state, now);
        match state.fragmentDecisions.get(&key).map(|entry| entry.action) {
            Some(FragmentAction::Allow) => FragmentLookup::Allow,
            Some(FragmentAction::Block { target, .. }) => FragmentLookup::Block(target),
            None => FragmentLookup::Unknown,
        }
    }

    /// 把首个重定向包的外部接口写入反射索引；代理回复必须使用该接口恢复为原始入站流。
    pub fn setReflectedInterface(
        &self,
        proxyAddress: IpAddr,
        reflectedPort: u16,
        remoteAddress: IpAddr,
        originalInterface: NetworkInterface,
    ) -> bool {
        self.setTransportReflectedInterface(
            tcpProtocol,
            proxyAddress,
            reflectedPort,
            remoteAddress,
            originalInterface,
        )
    }

    /// 写入指定传输协议的原始接口；TCP 地址反射与后续 UDP 扩展共享同一索引模型。
    pub(crate) fn setTransportReflectedInterface(
        &self,
        protocol: u8,
        proxyAddress: IpAddr,
        reflectedPort: u16,
        remoteAddress: IpAddr,
        originalInterface: NetworkInterface,
    ) -> bool {
        let key = ReflectedKey {
            localAddress: normalizeIpAddress(proxyAddress),
            localPort: reflectedPort,
            remoteAddress: normalizeIpAddress(remoteAddress),
            protocol,
        };
        let mut state = self.state.write().expect("流表写锁中毒");
        let Some(existing) = state.reflected.get(&key).copied() else {
            return false;
        };
        let endpointId = existing.endpointId;
        let scopedOriginal = withIpv6Scope(existing.original, originalInterface.interfaceIndex);
        if let Some(target) = state.reflected.get_mut(&key) {
            target.originalInterface = Some(originalInterface);
            target.original = scopedOriginal;
        }
        let outboundKeys = state
            .endpointKeys
            .get(&endpointId)
            .map(|keys| {
                keys.iter()
                    .map(|(outboundKey, _)| *outboundKey)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for outboundKey in outboundKeys {
            if let Some(target) = state.outbound.get_mut(&outboundKey) {
                target.rewrite.originalInterface = Some(originalInterface);
                target.rewrite.original = scopedOriginal;
            }
        }
        true
    }

    /// 查询原始出站数据包是否属于已选择进程；命中 SOCKET 通配地址时原子提升为真实 NETWORK 地址。
    pub(crate) fn outboundTarget(
        &self,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
    ) -> Option<RewriteTarget> {
        self.outboundTransportTarget(
            tcpProtocol,
            localAddress,
            localPort,
            remoteAddress,
            remotePort,
        )
    }

    /// 查询指定传输协议的原始出站五元组；UDP 首包可在 FLOW 事件到达后复用同一通配提升逻辑。
    pub(crate) fn outboundTransportTarget(
        &self,
        protocol: u8,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
    ) -> Option<RewriteTarget> {
        let key = OutboundKey {
            localAddress: normalizeIpAddress(localAddress),
            localPort,
            remoteAddress: normalizeIpAddress(remoteAddress),
            remotePort,
            protocol,
        };
        let mut state = self.state.write().expect("流表写锁中毒");
        if let Some(tracked) = state.outbound.get(&key) {
            return Some(tracked.rewrite);
        }
        let wildcardAddress = if key.localAddress.is_ipv4() {
            IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
        };
        let wildcardKey = OutboundKey {
            localAddress: wildcardAddress,
            ..key
        };
        let mut tracked = state.outbound.get(&wildcardKey).copied()?;
        let endpointId = tracked.endpointId;
        let reflectedKey = state.endpointKeys.get(&endpointId)?.iter().find_map(
            |(outboundKey, reflectedKey)| (*outboundKey == wildcardKey).then_some(*reflectedKey),
        )?;
        state.outbound.remove(&wildcardKey);
        tracked.rewrite.originalLocalAddress = key.localAddress;
        state.reflected.insert(reflectedKey, tracked.rewrite);
        state.outbound.insert(key, tracked);
        if let Some(keys) = state.endpointKeys.get_mut(&endpointId)
            && let Some((outboundKey, _)) = keys
                .iter_mut()
                .find(|(outboundKey, _)| *outboundKey == wildcardKey)
        {
            *outboundKey = key;
        }
        Some(tracked.rewrite)
    }

    /// 按拦截水位查询 UDP 流版本；当前与已关闭版本共享 QPC 区间，端口复用不得覆盖排队旧包。
    pub(crate) fn udpTargetAt(
        &self,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
        remotePort: u16,
        capturedAt: Instant,
        capturedAtCounter: i64,
    ) -> Option<RewriteTarget> {
        let key = OutboundKey {
            localAddress: normalizeIpAddress(localAddress),
            localPort,
            remoteAddress: normalizeIpAddress(remoteAddress),
            remotePort,
            protocol: udpProtocol,
        };
        let state = self.state.read().expect("流表读锁中毒");
        if let Some(current) = state.outbound.get(&key)
            && current
                .activeFromCounter
                .is_none_or(|activeFrom| capturedAtCounter >= activeFrom)
        {
            return Some(current.rewrite);
        }
        state.retiredUdpOutbound.get(&key).and_then(|versions| {
            versions
                .iter()
                .rev()
                .find(|retired| {
                    retired
                        .activeFromCounter
                        .is_none_or(|activeFrom| capturedAtCounter >= activeFrom)
                        && retired
                            .retiredAtCounter
                            .map_or(capturedAt <= retired.retiredAt, |retiredAt| {
                                capturedAtCounter < retiredAt
                            })
                })
                .map(|retired| retired.rewrite)
        })
    }

    /// 推进顺序 resolver 已完成的 QPC 水位；只有所有更早副本均已处理后才回收关闭流版本。
    pub(crate) fn advanceUdpObservationWatermark(&self, processedCounter: i64) {
        let mut state = self.state.write().expect("流表写锁中毒");
        state.retiredUdpOutbound.retain(|_, versions| {
            versions.retain(|retired| {
                retired
                    .retiredAtCounter
                    .is_none_or(|retiredAt| retiredAt >= processedCounter)
            });
            !versions.is_empty()
        });
    }

    /// 查询代理回复包对应的原目标；调用方使用目标端口恢复 TCP 源端口。
    pub(crate) fn reflectedTarget(
        &self,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
    ) -> Option<RewriteTarget> {
        self.reflectedTransportTarget(tcpProtocol, localAddress, localPort, remoteAddress)
    }

    /// 查询指定传输协议的反射索引；协议进入键空间，避免相同端口的 TCP 与 UDP 流互相误认。
    pub(crate) fn reflectedTransportTarget(
        &self,
        protocol: u8,
        localAddress: IpAddr,
        localPort: u16,
        remoteAddress: IpAddr,
    ) -> Option<RewriteTarget> {
        let key = ReflectedKey {
            localAddress: normalizeIpAddress(localAddress),
            localPort,
            remoteAddress: normalizeIpAddress(remoteAddress),
            protocol,
        };
        self.state
            .read()
            .expect("流表读锁中毒")
            .reflected
            .get(&key)
            .copied()
    }

    /// 由代理 `accept` 得到的 peer 恢复原目标；peer 端口是流表分配的唯一反射端口。
    pub fn originalTargetForPeer(
        &self,
        localAddress: IpAddr,
        peer: SocketAddr,
    ) -> Option<OriginalTarget> {
        self.reflectedTarget(localAddress, peer.port(), peer.ip())
            .map(|rewrite| rewrite.original)
    }

    /// 按反向五元组查询 UDP 响应所属目标进程；只读观察路径不改写数据报字节。
    #[cfg(test)]
    pub(crate) fn inboundTransportTarget(
        &self,
        protocol: u8,
        remoteAddress: IpAddr,
        remotePort: u16,
        localAddress: IpAddr,
        localPort: u16,
    ) -> Option<RewriteTarget> {
        let key = OutboundKey {
            localAddress: normalizeIpAddress(localAddress),
            localPort,
            remoteAddress: normalizeIpAddress(remoteAddress),
            remotePort,
            protocol,
        };
        self.state
            .read()
            .expect("流表读锁中毒")
            .outbound
            .get(&key)
            .map(|tracked| tracked.rewrite)
    }

    /// 返回当前已确认流数，供控制面快照和停止后的清零验证使用。
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.state.read().expect("流表读锁中毒").outbound.len()
    }

    /// 判断流表是否为空，不需要为状态快照复制索引。
    pub fn isEmpty(&self) -> bool {
        self.len() == 0
    }

    /// 停止捕获时清除会话局部五元组，避免重启后复用过期连接身份。
    pub fn clear(&self) {
        *self.state.write().expect("流表写锁中毒") = FlowState::default();
    }
}

/// 把通配监听地址解析为当前流的原本地地址，并拒绝无法由同一监听套接字承接的跨地址族流量。
fn resolveProxyAddress(
    originalLocalAddress: IpAddr,
    configuredProxyAddress: IpAddr,
) -> Option<IpAddr> {
    let configuredProxyAddress = normalizeIpAddress(configuredProxyAddress);
    if configuredProxyAddress.is_unspecified() {
        return Some(if originalLocalAddress.is_ipv4() {
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        });
    }
    (configuredProxyAddress.is_ipv4() == originalLocalAddress.is_ipv4())
        .then_some(configuredProxyAddress)
}

/// 为 IPv6 链路本地目标补充首个外发包携带的接口范围；全局地址和 IPv4 保持原样。
pub(crate) fn withIpv6Scope(target: OriginalTarget, interfaceIndex: u32) -> OriginalTarget {
    let address = match target.address {
        SocketAddr::V6(address)
            if address.ip().is_unicast_link_local() && address.scope_id() == 0 =>
        {
            SocketAddr::V6(std::net::SocketAddrV6::new(
                *address.ip(),
                address.port(),
                address.flowinfo(),
                interfaceIndex,
            ))
        }
        address => address,
    };
    OriginalTarget {
        processId: target.processId,
        address,
    }
}

/// 在持有唯一写锁时移除端点，保证所有索引具有相同生命周期。
fn removeEndpointLocked(state: &mut FlowState, endpointId: u64, retiredAtCounter: Option<i64>) {
    let now = Instant::now();
    if let Some(keys) = state.endpointKeys.remove(&endpointId) {
        for (outboundKey, reflectedKey) in keys {
            if let Some(tracked) = state.outbound.remove(&outboundKey)
                && outboundKey.protocol == udpProtocol
            {
                state
                    .retiredUdpOutbound
                    .entry(outboundKey)
                    .or_default()
                    .push(RetiredUdpTarget {
                        rewrite: tracked.rewrite,
                        activeFromCounter: tracked.activeFromCounter,
                        retiredAt: now,
                        retiredAtCounter,
                    });
            }
            state.reflected.remove(&reflectedKey);
        }
    }
    removeUdpBindingLocked(state, endpointId);
    state.fragmentDecisions.retain(|_, decision| {
        !matches!(
            decision.action,
            FragmentAction::Block {
                endpointId: decisionEndpointId,
                ..
            } if decisionEndpointId == endpointId
        )
    });
}

/// 在持有流表写锁时删除 UDP BIND 反向索引；未知端点表示 BIND 尚未登记或已被 CLOSE 回收。
fn removeUdpBindingLocked(state: &mut FlowState, endpointId: u64) {
    let Some(bindingKey) = state.udpBindingKeys.remove(&endpointId) else {
        return;
    };
    let removeBindingKey = state
        .udpBindings
        .get_mut(&bindingKey)
        .is_some_and(|bindings| {
            bindings.remove(&endpointId);
            bindings.is_empty()
        });
    if removeBindingKey {
        state.udpBindings.remove(&bindingKey);
    }
}

/// 回收已过期分片决策；调用方持有流表写锁，避免清理与同键更新竞态。
fn pruneFragmentState(state: &mut FlowState, now: Instant) {
    state
        .fragmentDecisions
        .retain(|_, decision| decision.expiresAt > now);
}

/// 分配不会与现有反射五元组冲突的源端口；端口空间耗尽表示系统连接数已超出 TCP 上限。
fn allocateReflectedPort(
    state: &mut FlowState,
    localAddress: IpAddr,
    remoteAddress: IpAddr,
    proxyPort: u16,
    protocol: u8,
) -> Option<u16> {
    for _ in 0..u16::MAX {
        let candidate = state.nextReflectedPort;
        state.nextReflectedPort = if candidate == u16::MAX {
            1
        } else {
            candidate + 1
        };
        let key = ReflectedKey {
            localAddress,
            localPort: candidate,
            remoteAddress,
            protocol,
        };
        if candidate != proxyPort && !state.reflected.contains_key(&key) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建确定性测试流，覆盖五元组、反射索引和端点清理的一致性。
    fn testFlow() -> CaptureFlow {
        CaptureFlow::tcp(
            4200,
            91,
            "192.0.2.10".parse().unwrap(),
            52100,
            "198.51.100.20".parse().unwrap(),
            443,
        )
        .unwrap()
    }

    #[test]
    /// 验证三张索引同步创建和删除，并确认透明监听查询得到原目标。
    fn indexesAndRemovesFlowAtomically() {
        let table = CaptureFlowTable::default();
        table.insert(testFlow(), "127.0.0.1".parse().unwrap(), 1080);
        assert_eq!(
            table
                .outboundTarget(
                    "192.0.2.10".parse().unwrap(),
                    52100,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .unwrap()
                .original
                .address,
            "198.51.100.20:443".parse().unwrap()
        );
        let reflectedPort = table
            .outboundTarget(
                "192.0.2.10".parse().unwrap(),
                52100,
                "198.51.100.20".parse().unwrap(),
                443,
            )
            .unwrap()
            .reflectedPort;
        let originalInterface = NetworkInterface {
            interfaceIndex: 12,
            subinterfaceIndex: 3,
        };
        assert!(table.setReflectedInterface(
            "127.0.0.1".parse().unwrap(),
            reflectedPort,
            "127.0.0.1".parse().unwrap(),
            originalInterface,
        ));
        assert_eq!(
            table
                .reflectedTarget(
                    "127.0.0.1".parse().unwrap(),
                    reflectedPort,
                    "127.0.0.1".parse().unwrap(),
                )
                .unwrap()
                .originalInterface,
            Some(originalInterface)
        );
        assert_eq!(
            table
                .originalTargetForPeer(
                    "127.0.0.1".parse().unwrap(),
                    SocketAddr::new("127.0.0.1".parse().unwrap(), reflectedPort),
                )
                .unwrap()
                .processId,
            4200
        );
        let fragmentKey = FragmentKey {
            sourceAddress: "192.0.2.10".parse().unwrap(),
            destinationAddress: "198.51.100.20".parse().unwrap(),
            identification: 7,
            protocol: tcpProtocol,
            outbound: true,
        };
        assert_eq!(
            table.recordFragmentDecision(
                fragmentKey,
                FragmentAction::Block {
                    endpointId: 91,
                    target: OriginalTarget {
                        processId: 4200,
                        address: "198.51.100.20:443".parse().unwrap(),
                    },
                },
            ),
            FragmentRecordResult::Recorded
        );
        assert!(matches!(
            table.fragmentLookup(fragmentKey),
            FragmentLookup::Block(_)
        ));
        table.removeEndpoint(91);
        assert!(table.isEmpty());
        assert_eq!(table.fragmentLookup(fragmentKey), FragmentLookup::Unknown);
        assert_eq!(
            table.recordFragmentDecision(
                fragmentKey,
                FragmentAction::Block {
                    endpointId: 91,
                    target: OriginalTarget {
                        processId: 4200,
                        address: "198.51.100.20:443".parse().unwrap(),
                    },
                },
            ),
            FragmentRecordResult::EndpointGone
        );
    }

    #[test]
    /// 验证 PID 热替换只删除已取消选择的进程，并保持其它进程全部反射索引可查询。
    fn retainsOnlyCurrentlySelectedProcesses() {
        let table = CaptureFlowTable::default();
        let first = testFlow();
        let second = CaptureFlow::tcp(
            4300,
            92,
            "192.0.2.10".parse().unwrap(),
            52101,
            "198.51.100.21".parse().unwrap(),
            443,
        )
        .unwrap();
        assert!(table.insert(first, "127.0.0.1".parse().unwrap(), 1080));
        assert!(table.insert(second, "127.0.0.1".parse().unwrap(), 1080));

        table.retainProcessIds(&BTreeSet::from([4300]));

        assert_eq!(table.len(), 1);
        assert!(
            table
                .outboundTarget(
                    "192.0.2.10".parse().unwrap(),
                    52100,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .is_none()
        );
        assert_eq!(
            table
                .outboundTarget(
                    "192.0.2.10".parse().unwrap(),
                    52101,
                    "198.51.100.21".parse().unwrap(),
                    443,
                )
                .unwrap()
                .original
                .processId,
            4300
        );
    }

    #[test]
    /// 验证通配监听保留原本地地址，而跨地址族监听不会登记无法送达的透明流。
    fn resolvesWildcardAndRejectsAddressFamilyMismatch() {
        let wildcardTable = CaptureFlowTable::default();
        assert!(wildcardTable.insert(testFlow(), "0.0.0.0".parse().unwrap(), 1080));
        let target = wildcardTable
            .outboundTarget(
                "192.0.2.10".parse().unwrap(),
                52100,
                "198.51.100.20".parse().unwrap(),
                443,
            )
            .unwrap();
        assert_eq!(target.proxyAddress, "127.0.0.1".parse::<IpAddr>().unwrap());

        let ipv6Table = CaptureFlowTable::default();
        let ipv6Flow = CaptureFlow::tcp(
            4200,
            93,
            "2001:db8::10".parse().unwrap(),
            52102,
            "2001:db8::20".parse().unwrap(),
            443,
        )
        .unwrap();
        assert!(ipv6Table.insert(ipv6Flow, "::".parse().unwrap(), 1080));
        assert_eq!(
            ipv6Table
                .outboundTarget(
                    "2001:db8::10".parse().unwrap(),
                    52102,
                    "2001:db8::20".parse().unwrap(),
                    443,
                )
                .unwrap()
                .proxyAddress,
            "::1".parse::<IpAddr>().unwrap()
        );

        let mismatchedTable = CaptureFlowTable::default();
        assert!(!mismatchedTable.insert(testFlow(), "::1".parse().unwrap(), 1080));
        assert!(mismatchedTable.isEmpty());
    }

    #[test]
    /// 验证 SOCKET CONNECT 的通配本地地址会被首个真实数据包原子提升，并同步更新回复恢复地址。
    fn promotesWildcardSocketAssociationFromNetworkTuple() {
        let table = CaptureFlowTable::default();
        let wildcardFlow = CaptureFlow::tcp(
            4200,
            92,
            "0.0.0.0".parse().unwrap(),
            52101,
            "198.51.100.20".parse().unwrap(),
            443,
        )
        .unwrap();
        assert!(table.insert(wildcardFlow, "127.0.0.1".parse().unwrap(), 1080));

        let promoted = table
            .outboundTarget(
                "192.0.2.10".parse().unwrap(),
                52101,
                "198.51.100.20".parse().unwrap(),
                443,
            )
            .expect("真实 NETWORK 五元组应提升通配关联");
        assert_eq!(
            promoted.originalLocalAddress,
            "192.0.2.10".parse::<IpAddr>().unwrap()
        );
        assert!(
            table
                .outboundTarget(
                    "0.0.0.0".parse().unwrap(),
                    52101,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .is_none()
        );
        assert_eq!(
            table
                .reflectedTarget(
                    "127.0.0.1".parse().unwrap(),
                    promoted.reflectedPort,
                    "127.0.0.1".parse().unwrap(),
                )
                .unwrap()
                .originalLocalAddress,
            "192.0.2.10".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    /// 验证反射端口跳过融合监听端口，并在端口空间轮转后仍不会选择保留端口。
    fn skipsProxyPortDuringReflectedPortAllocation() {
        let firstTable = CaptureFlowTable::default();
        assert!(firstTable.insert(testFlow(), "127.0.0.1".parse().unwrap(), firstReflectedPort,));
        assert_eq!(
            firstTable
                .outboundTarget(
                    "192.0.2.10".parse().unwrap(),
                    52100,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .unwrap()
                .reflectedPort,
            firstReflectedPort + 1
        );

        let wrappedTable = CaptureFlowTable::default();
        wrappedTable
            .state
            .write()
            .expect("测试流表写锁不得中毒")
            .nextReflectedPort = u16::MAX;
        assert!(wrappedTable.insert(testFlow(), "127.0.0.1".parse().unwrap(), u16::MAX,));
        assert_eq!(
            wrappedTable
                .outboundTarget(
                    "192.0.2.10".parse().unwrap(),
                    52100,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .unwrap()
                .reflectedPort,
            1
        );
    }

    #[test]
    /// 验证满表时稳定淘汰同过期时刻的最小键，且未选流与未知后续片始终保持旁路。
    fn boundsAndExpiresFragmentDecisions() {
        let table = CaptureFlowTable::default();
        assert!(table.insert(testFlow(), "127.0.0.1".parse().unwrap(), 1080));
        let now = Instant::now();
        for identification in 0..maximumFragmentDecisions {
            assert_eq!(
                table.recordFragmentDecisionAt(
                    FragmentKey {
                        sourceAddress: "192.0.2.10".parse().unwrap(),
                        destinationAddress: "198.51.100.20".parse().unwrap(),
                        identification: u32::try_from(identification).unwrap(),
                        protocol: tcpProtocol,
                        outbound: true,
                    },
                    FragmentAction::Allow,
                    now,
                ),
                FragmentRecordResult::Recorded
            );
        }
        let overflowKey = FragmentKey {
            sourceAddress: "192.0.2.11".parse().unwrap(),
            destinationAddress: "198.51.100.21".parse().unwrap(),
            identification: u32::MAX,
            protocol: tcpProtocol,
            outbound: true,
        };
        assert_eq!(
            table.recordFragmentDecisionAt(overflowKey, FragmentAction::Allow, now),
            FragmentRecordResult::Recorded
        );
        let deterministicallyEvictedKey = FragmentKey {
            sourceAddress: "192.0.2.10".parse().unwrap(),
            destinationAddress: "198.51.100.20".parse().unwrap(),
            identification: 0,
            protocol: tcpProtocol,
            outbound: true,
        };
        let retainedKey = FragmentKey {
            identification: 1,
            ..deterministicallyEvictedKey
        };
        assert_eq!(
            table.fragmentLookupAt(deterministicallyEvictedKey, now),
            FragmentLookup::Unknown
        );
        assert_eq!(
            table.fragmentLookupAt(retainedKey, now),
            FragmentLookup::Allow
        );
        assert_eq!(
            table.fragmentLookupAt(overflowKey, now),
            FragmentLookup::Allow
        );
        let unrelatedUnknownKey = FragmentKey {
            sourceAddress: "203.0.113.50".parse().unwrap(),
            destinationAddress: "203.0.113.51".parse().unwrap(),
            identification: 77,
            protocol: tcpProtocol,
            outbound: true,
        };
        assert_eq!(
            table.fragmentLookupAt(unrelatedUnknownKey, now),
            FragmentLookup::Unknown
        );
        assert_eq!(
            table
                .state
                .read()
                .expect("测试流表读锁不得中毒")
                .fragmentDecisions
                .len(),
            maximumFragmentDecisions
        );
        assert_eq!(
            table.fragmentLookupAt(overflowKey, now + fragmentDecisionLifetime),
            FragmentLookup::Unknown
        );
        assert_eq!(
            table.recordFragmentDecisionAt(
                overflowKey,
                FragmentAction::Allow,
                now + fragmentDecisionLifetime,
            ),
            FragmentRecordResult::Recorded
        );
    }

    #[test]
    /// 验证回环目标和缺少本地端口的事件不会进入捕获流表。
    fn rejectsLoopbackAndIncompleteFlows() {
        assert!(
            CaptureFlow::tcp(
                1,
                1,
                "127.0.0.1".parse().unwrap(),
                5000,
                "127.0.0.1".parse().unwrap(),
                80
            )
            .is_none()
        );
        assert!(
            CaptureFlow::tcp(
                1,
                1,
                "192.0.2.1".parse().unwrap(),
                0,
                "198.51.100.1".parse().unwrap(),
                80
            )
            .is_none()
        );
        assert!(
            CaptureFlow::tcp(
                1,
                1,
                "192.0.2.1".parse().unwrap(),
                5000,
                "2001:db8::1".parse().unwrap(),
                80
            )
            .is_none()
        );
    }

    #[test]
    /// 验证无连接 UDP 的通配 BIND 能在首包时建立多个远端五元组，并由一次 CLOSE 全量回收。
    fn associatesMultipleUdpTargetsFromSingleBinding() {
        let table = CaptureFlowTable::default();
        assert!(table.registerUdpBinding(7001, 81, "0.0.0.0".parse().unwrap(), 53000,));
        let first = table
            .associateUdpOutbound(
                "192.0.2.10".parse().unwrap(),
                53000,
                "198.51.100.10".parse().unwrap(),
                443,
                "0.0.0.0".parse().unwrap(),
                1080,
            )
            .expect("首个 UDP 目标应由 BIND 归属");
        let second = table
            .associateUdpOutbound(
                "192.0.2.10".parse().unwrap(),
                53000,
                "198.51.100.11".parse().unwrap(),
                53,
                "0.0.0.0".parse().unwrap(),
                1080,
            )
            .expect("同一 UDP socket 的第二目标应独立建表");
        assert_eq!(first.original.processId, 7001);
        assert_eq!(second.original.processId, 7001);
        assert_eq!(table.len(), 2);
        assert!(
            table
                .inboundTransportTarget(
                    udpProtocol,
                    "198.51.100.10".parse().unwrap(),
                    443,
                    "192.0.2.10".parse().unwrap(),
                    53000,
                )
                .is_some()
        );
        table.removeEndpoint(81);
        assert_eq!(table.len(), 0);
        assert!(
            table
                .associateUdpOutbound(
                    "192.0.2.10".parse().unwrap(),
                    53000,
                    "198.51.100.12".parse().unwrap(),
                    123,
                    "0.0.0.0".parse().unwrap(),
                    1080,
                )
                .is_none()
        );
    }

    #[test]
    /// 验证共享同一 UDP 本地端口的多个选中进程保持旁路，禁止把首包错误归属给任意一个 PID。
    fn rejectsAmbiguousUdpBindingOwnership() {
        let table = CaptureFlowTable::default();
        assert!(table.registerUdpBinding(7001, 91, "0.0.0.0".parse().unwrap(), 53001));
        assert!(table.registerUdpBinding(7002, 92, "0.0.0.0".parse().unwrap(), 53001));
        assert!(
            table
                .associateUdpOutbound(
                    "192.0.2.10".parse().unwrap(),
                    53001,
                    "198.51.100.10".parse().unwrap(),
                    443,
                    "0.0.0.0".parse().unwrap(),
                    1080,
                )
                .is_none()
        );
    }

    #[test]
    /// 验证同一 UDP 五元组跨进程复用时按 QPC 区间归属，排队超过旧固定时限仍不得命中新代际。
    fn selectsUdpGenerationByCaptureCounter() {
        let table = CaptureFlowTable::default();
        let localAddress = "192.0.2.10".parse().unwrap();
        let remoteAddress = "198.51.100.10".parse().unwrap();
        let first = CaptureFlow::udp(7001, 101, localAddress, 53002, remoteAddress, 443).unwrap();
        let second = CaptureFlow::udp(7002, 102, localAddress, 53002, remoteAddress, 443).unwrap();
        assert!(table.insertAt(first, "127.0.0.1".parse().unwrap(), 1080, Some(100)));
        table.removeEndpointsAt(&[101], 200);
        assert!(table.insertAt(second, "127.0.0.1".parse().unwrap(), 1080, Some(300)));

        let delayedCapture = Instant::now() - Duration::from_secs(31);
        assert_eq!(
            table
                .udpTargetAt(localAddress, 53002, remoteAddress, 443, delayedCapture, 150,)
                .unwrap()
                .original
                .processId,
            7001
        );
        assert_eq!(
            table
                .udpTargetAt(localAddress, 53002, remoteAddress, 443, Instant::now(), 350,)
                .unwrap()
                .original
                .processId,
            7002
        );
        table.advanceUdpObservationWatermark(350);
        assert!(
            table
                .udpTargetAt(localAddress, 53002, remoteAddress, 443, delayedCapture, 150,)
                .is_none()
        );
    }

    #[test]
    /// 验证 IPv6 链路本地目标使用真实外发接口作为 scope，全局 IPv6 地址不附加范围。
    fn scopesOnlyIpv6LinkLocalTargets() {
        let linkLocal = OriginalTarget {
            processId: 1,
            address: "[fe80::1]:443".parse().unwrap(),
        };
        let scoped = withIpv6Scope(linkLocal, 58);
        let SocketAddr::V6(scopedAddress) = scoped.address else {
            panic!("链路本地目标必须保持 IPv6")
        };
        assert_eq!(scopedAddress.scope_id(), 58);
        let global = OriginalTarget {
            processId: 1,
            address: "[2001:db8::1]:443".parse().unwrap(),
        };
        assert_eq!(withIpv6Scope(global, 58), global);
    }
}
