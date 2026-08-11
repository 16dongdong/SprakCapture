//! 承载 SOCKET/FLOW 生命周期事件消费线程。
//!
//! 进程选择更新锁把事件登记与热更新发布串行，防止删除后的端点被旧事件重新写回。

use super::*;

/// 运行 SOCKET 观察线程；`context` 统一提供当前捕获代际的进程集合、反射入口与累计指标。
/// CONNECT 先于 SYN 建表并只在首次登记时增加已接受连接，CLOSE 立即释放端点索引；线程创建失败返回 Worker 错误。
pub(super) fn spawnSocketWorker(
    divert: WinDivert<windivert::layer::SocketLayer>,
    context: SocketWorkerContext,
) -> Result<JoinHandle<()>, ProcessCaptureError> {
    thread::Builder::new()
        .name("process-capture-socket".to_owned())
        .spawn(move || {
            loop {
                if context.stopRequested.load(Ordering::Acquire) {
                    break;
                }
                match divert.recv() {
                    Ok(packet) if matches!(packet.address.event(), WinDivertEvent::SocketBind) => {
                        let address = &packet.address;
                        let _selectionUpdate = context
                            .processSelectionUpdateLock
                            .lock()
                            .expect("进程选择更新锁中毒");
                        if address.protocol() == crate::flowTable::udpProtocol
                            && context
                                .selectedProcessIds
                                .read()
                                .expect("选中进程集合读锁中毒")
                                .contains(&address.process_id())
                        {
                            let _ = context.flowTable.registerUdpBinding(
                                address.process_id(),
                                address.endpoint_id(),
                                address.local_address(),
                                address.local_port(),
                            );
                        }
                    }
                    Ok(packet)
                        if matches!(packet.address.event(), WinDivertEvent::SocketConnect) =>
                    {
                        let _selectionUpdate = context
                            .processSelectionUpdateLock
                            .lock()
                            .expect("进程选择更新锁中毒");
                        if let Some(flow) = socketFlow(&packet.address)
                            && context
                                .selectedProcessIds
                                .read()
                                .expect("选中进程集合读锁中毒")
                                .contains(&flow.processId)
                            && context.flowTable.insertAt(
                                flow,
                                context.proxyAddress,
                                context.proxyPort,
                                Some(packet.address.event_timestamp()),
                            )
                        {
                            context.acceptedConnections.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(packet) if matches!(packet.address.event(), WinDivertEvent::SocketClose) => {
                        let _selectionUpdate = context
                            .processSelectionUpdateLock
                            .lock()
                            .expect("进程选择更新锁中毒");
                        if packet.address.protocol() == crate::flowTable::udpProtocol {
                            removeSyntheticUdpBinding(
                                &context.flowTable,
                                &context.ownerUdpEndpoints,
                                OwnedUdpBinding {
                                    processId: packet.address.process_id(),
                                    localAddress: packet.address.local_address(),
                                    localPort: packet.address.local_port(),
                                },
                                packet.address.event_timestamp(),
                            );
                        }
                        context.flowTable.removeEndpointsAt(
                            &[packet.address.endpoint_id()],
                            packet.address.event_timestamp(),
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if context.stopRequested.load(Ordering::Acquire) {
                            break;
                        }
                        recordWorkerError(&context.lastError, "SOCKET", error.to_string());
                        break;
                    }
                }
            }
        })
        .map_err(|error| ProcessCaptureError::Worker {
            worker: "SOCKET 创建",
            detail: error.to_string(),
        })
}

/// 运行 FLOW 生命周期线程；ESTABLISHED 用最终五元组补强 SOCKET 关联，DELETED 回收端点。
pub(super) fn spawnFlowWorker(
    divert: WinDivert<windivert::layer::FlowLayer>,
    context: SocketWorkerContext,
) -> Result<JoinHandle<()>, ProcessCaptureError> {
    thread::Builder::new()
        .name("process-capture-flow".to_owned())
        .spawn(move || {
            loop {
                if context.stopRequested.load(Ordering::Acquire) {
                    break;
                }
                match divert.recv() {
                    Ok(packet)
                        if matches!(packet.address.event(), WinDivertEvent::FlowEstablished) =>
                    {
                        let _selectionUpdate = context
                            .processSelectionUpdateLock
                            .lock()
                            .expect("进程选择更新锁中毒");
                        if let Some(flow) = flowLayerFlow(&packet.address) {
                            if !context
                                .selectedProcessIds
                                .read()
                                .expect("选中进程集合读锁中毒")
                                .contains(&flow.processId)
                            {
                                continue;
                            }
                            if flow.protocol == crate::flowTable::udpProtocol {
                                // owner 快照端点没有真实 endpointId；权威 FLOW 到达后按 PID/本地端点
                                // 回收 synthetic 及其派生五元组，再由真实 endpoint 接管生命周期。
                                removeSyntheticUdpBinding(
                                    &context.flowTable,
                                    &context.ownerUdpEndpoints,
                                    OwnedUdpBinding {
                                        processId: flow.processId,
                                        localAddress: flow.localAddress,
                                        localPort: flow.localPort,
                                    },
                                    packet.address.event_timestamp(),
                                );
                                // 无连接 UDP 的 `sendto` 不一定产生 SOCKET CONNECT；FLOW ESTABLISHED
                                // 是首个数据报与 PID 建立精确归属的权威事件；已由 SOCKET CONNECT
                                // 登记的连接式 UDP 只保留原索引，避免重复计数和反射端口漂移。
                                let existing = context.flowTable.outboundTransportTarget(
                                    crate::flowTable::udpProtocol,
                                    flow.localAddress,
                                    flow.localPort,
                                    flow.remoteAddress,
                                    flow.remotePort,
                                );
                                if existing.is_none()
                                    && context.flowTable.insertAt(
                                        flow,
                                        context.proxyAddress,
                                        context.proxyPort,
                                        Some(packet.address.event_timestamp()),
                                    )
                                {
                                    context.acceptedConnections.fetch_add(1, Ordering::Relaxed);
                                }
                            } else {
                                // TCP ESTABLISHED 已晚于握手，禁止中途新建反射映射；只提升 SOCKET 通配地址。
                                let _ = context.flowTable.outboundTarget(
                                    flow.localAddress,
                                    flow.localPort,
                                    flow.remoteAddress,
                                    flow.remotePort,
                                );
                            }
                        }
                    }
                    Ok(packet) if matches!(packet.address.event(), WinDivertEvent::FlowDeleted) => {
                        let _selectionUpdate = context
                            .processSelectionUpdateLock
                            .lock()
                            .expect("进程选择更新锁中毒");
                        if packet.address.protocol() == crate::flowTable::udpProtocol {
                            removeSyntheticUdpBinding(
                                &context.flowTable,
                                &context.ownerUdpEndpoints,
                                OwnedUdpBinding {
                                    processId: packet.address.process_id(),
                                    localAddress: packet.address.local_address(),
                                    localPort: packet.address.local_port(),
                                },
                                packet.address.event_timestamp(),
                            );
                        }
                        context.flowTable.removeEndpointsAt(
                            &[packet.address.endpoint_id()],
                            packet.address.event_timestamp(),
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if context.stopRequested.load(Ordering::Acquire) {
                            break;
                        }
                        recordWorkerError(&context.lastError, "FLOW", error.to_string());
                        break;
                    }
                }
            }
        })
        .map_err(|error| ProcessCaptureError::Worker {
            worker: "FLOW 创建",
            detail: error.to_string(),
        })
}
