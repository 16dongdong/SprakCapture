#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use capture_core::{MessageSide, RecordingConfiguration, RecordingSession, TransactionProtocol};
use process_capture_core::{
    ProcessCapture, ProcessCaptureConfiguration, UdpDatagramDirection, UdpDatagramEvent,
    UdpDatagramModification, UdpDatagramSink,
};
use proxy_backend::udpRecording::{
    SpoolSegmentRemover, UdpProcessNameResolver, UdpRecordingCoordination, UdpRecordingSpool,
    captureQueueCapacity, createUdpRecordingPipeline, spawnSpoolWriter,
    startCoordinatedUdpRecordingGeneration, startUdpRecordingGeneration,
};
#[cfg(windows)]
use std::fs::OpenOptions;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, ToSocketAddrs, UdpSocket},
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tokio::time::timeout;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

const stressDatagrams: u32 = 2_048;
const recordingTimeout: Duration = Duration::from_secs(3);
const recordingGeneration: &str = "udp-test-process-generation";
const realDriverHelperEnvironment: &str = "SPRAK_UDP_SPOOL_DRIVER_HELPER";
const realDriverTargetEnvironment: &str = "SPRAK_UDP_SPOOL_DRIVER_TARGET";
const realDriverDnsTargetEnvironment: &str = "SPRAK_UDP_SPOOL_DRIVER_DNS_TARGET";

/// 跳过 Rust 测试 harness 自身输出并等待 helper 的单字节 BIND ready 标记。
fn waitForReadySignal(reader: &mut impl Read) {
    let mut byte = [0_u8; 1];
    for _ in 0..4_096 {
        reader
            .read_exact(&mut byte)
            .expect("读取 UDP BIND ready 输出");
        if byte == *b"R" {
            return;
        }
    }
    panic!("UDP helper 在输出上限内未报告 BIND ready");
}

/// 读取 Windows 当前生效的首个 IPv4 DNS 服务器，真实 child 直接向该端点发送可核对查询。
fn systemIpv4DnsTarget() -> SocketAddr {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "Get-DnsClientServerAddress -AddressFamily IPv4 | ForEach-Object ServerAddresses | Where-Object { $_ -and $_ -ne '127.0.0.1' } | Select-Object -First 1",
        ])
        .output()
        .expect("枚举系统 IPv4 DNS 服务器");
    assert!(output.status.success(), "系统 DNS 枚举失败：{output:?}");
    let address = String::from_utf8(output.stdout)
        .expect("系统 DNS 输出不是 UTF-8")
        .trim()
        .parse()
        .expect("系统 DNS 地址格式错误");
    SocketAddr::new(address, 53)
}

/// 枚举录制目录中的正文分段；确认游标和原子替换临时文件不计入结果。
fn spoolFiles(directory: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("spool") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// 测试事务无需依赖瞬时系统进程名，使用稳定空解析器隔离 PID 回收噪声。
fn emptyProcessNameResolver() -> Arc<dyn UdpProcessNameResolver> {
    Arc::new(|_processId: u32| None)
}

/// 验证 WinDivert 双栈 UDP 事件形成方向明确、正文完整且携带进程身份的可见事务。
#[tokio::test]
async fn recordsIpv4AndIpv6UdpDatagramsWithoutTruncation() {
    timeout(recordingTimeout, async {
        let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 录制临时目录");
        let recording = RecordingSession::new(RecordingConfiguration {
            spillDirectory: temporaryDirectory.path().to_path_buf(),
            memoryBodyThreshold: 1,
            ..RecordingConfiguration::default()
        })
        .await
        .expect("创建 UDP 录制会话");
        let requestPayload = b"complete-ipv4-udp-request".to_vec();
        proxy_backend::udpRecording::recordUdpDatagram(
            &recording,
            Some("udp-client.exe".to_owned()),
            UdpDatagramEvent {
                processId: 41_001,
                clientAddress: "192.0.2.10:53000".parse().unwrap(),
                targetAddress: "198.51.100.20:443".parse().unwrap(),
                direction: UdpDatagramDirection::Up,
                payload: requestPayload.clone(),
                capturedAtMilliseconds: 100,
                modifications: vec![UdpDatagramModification {
                    offsetBytes: 3,
                    originalBytes: vec![0x01],
                    modifiedBytes: vec![0x00],
                }],
            },
        )
        .await;
        let responsePayload = b"complete-ipv6-udp-response".to_vec();
        proxy_backend::udpRecording::recordUdpDatagram(
            &recording,
            Some("udp-client.exe".to_owned()),
            UdpDatagramEvent {
                processId: 41_001,
                clientAddress: "[2001:db8::10]:53001".parse().unwrap(),
                targetAddress: "[2001:db8::20]:53".parse().unwrap(),
                direction: UdpDatagramDirection::Down,
                payload: responsePayload.clone(),
                capturedAtMilliseconds: 101,
                modifications: Vec::new(),
            },
        )
        .await;

        let summaries = recording.listMetadata().await.expect("读取 UDP 录制摘要");
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().all(|summary| {
            summary.protocol == TransactionProtocol::Tunnel
                && matches!(summary.method.as_str(), "UDP SEND" | "UDP RECEIVE")
                && summary.clientProcessName.as_deref() == Some("udp-client.exe")
                && summary.clientProcessId == Some(41_001)
                && !summary.flags.bodyTruncated
        }));
        let request = summaries
            .iter()
            .find(|summary| summary.urlDisplay == "udp://198.51.100.20:443")
            .expect("IPv4 UDP 请求事务可见");
        assert_eq!(request.method, "UDP SEND");
        assert_eq!(request.sizes.requestBodyBytes, requestPayload.len() as u64);
        assert_eq!(
            recording
                .getBody(&request.transactionId, MessageSide::Request)
                .await
                .expect("读取 IPv4 UDP 请求正文")
                .bytes,
            requestPayload
        );
        let requestDetail = recording
            .getTransactionDetail(&request.transactionId)
            .await
            .expect("读取 IPv4 UDP 请求详情");
        assert_eq!(requestDetail.requestPackets.len(), 1);
        assert_eq!(requestDetail.requestPackets[0].modifications.len(), 1);
        assert_eq!(
            requestDetail.requestPackets[0].modifications[0].originalBytes,
            vec![0x01]
        );
        assert_eq!(
            requestDetail.requestPackets[0].modifications[0].modifiedBytes,
            vec![0x00]
        );
        let response = summaries
            .iter()
            .find(|summary| summary.urlDisplay == "udp://[2001:db8::20]:53")
            .expect("IPv6 UDP 响应事务可见");
        assert_eq!(response.method, "UDP RECEIVE");
        assert_eq!(
            response.sizes.responseBodyBytes,
            responsePayload.len() as u64
        );
        assert_eq!(
            recording
                .getBody(&response.transactionId, MessageSide::Response)
                .await
                .expect("读取 IPv6 UDP 响应正文")
                .bytes,
            responsePayload
        );
    })
    .await
    .expect("UDP 录制测试超时");
}

/// 验证完整代际在生产与消费并发时保持顺序，并在 shutdown 后排空 FIFO 与全部分段。
#[tokio::test]
async fn drainsDiskSpoolIntoRecordingWithoutReorderingOrLoss() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP spool 压力测试目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording"),
        memoryBodyThreshold: 1,
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建 UDP spool 压力录制会话");
    let runtime = startUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        recordingGeneration,
    )
    .expect("启动 UDP spool 压力代际");
    let sink = runtime.sink();
    let producerSink = Arc::clone(&sink);
    let producer = thread::spawn(move || {
        for sequence in 0..stressDatagrams {
            let mut payload = vec![0_u8; 4 * 1024];
            payload[..4].copy_from_slice(&sequence.to_be_bytes());
            producerSink
                .append(UdpDatagramEvent {
                    processId: 41_001,
                    clientAddress: "192.0.2.10:53000".parse().unwrap(),
                    targetAddress: "198.51.100.20:443".parse().unwrap(),
                    direction: UdpDatagramDirection::Up,
                    payload,
                    capturedAtMilliseconds: u64::from(sequence),
                    modifications: Vec::new(),
                })
                .expect("高负载事件应进入固定内存队列");
            // 让生产持续跨越多个调度周期，测试才能证明 reader 在 writer 尚未关闭时
            // 实时提交，而不是在 stop 后一次性补录；速率仍远高于事务落盘消费者。
            if sequence % 4 == 3 {
                thread::sleep(Duration::from_millis(1));
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut sawLiveRecording = false;
    while !producer.is_finished() && Instant::now() < deadline {
        sawLiveRecording |= !recording
            .listMetadata()
            .await
            .expect("读取生产中的 UDP spool 事务")
            .is_empty();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    producer.join().expect("等待 UDP spool 压力生产者");
    assert!(sawLiveRecording, "生产尚未停止时必须已经提交录制事务");
    runtime
        .stopAndDrain()
        .await
        .expect("停止并排空 UDP spool 压力代际");
    assert!(sink.fault().is_none(), "UDP spool 压力代际不应故障");
    let metadata = recording
        .listMetadata()
        .await
        .expect("读取 UDP spool 压力事务");
    assert_eq!(metadata.len(), usize::try_from(stressDatagrams).unwrap());
    assert!(
        metadata
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(
        spoolFiles(&temporaryDirectory.path().join("capture"))
            .expect("枚举已排空 UDP spool 分段")
            .is_empty()
    );
}

/// 验证内存队列满载时当前包进入唯一 emergency，故障可见且 writer 最终按序保存全部已接受事件。
#[test]
fn preservesEmergencyPacketWhenCaptureQueueIsFull() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 队列满载测试目录");
    let (sink, spool, captureReceiver) =
        createUdpRecordingPipeline(temporaryDirectory.path(), recordingGeneration)
            .expect("创建 UDP 队列满载管线");
    for sequence in 0..captureQueueCapacity {
        sink.append(UdpDatagramEvent {
            processId: 41_002,
            clientAddress: "192.0.2.11:53001".parse().unwrap(),
            targetAddress: "198.51.100.21:53".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: u32::try_from(sequence).unwrap().to_be_bytes().to_vec(),
            capturedAtMilliseconds: u64::try_from(sequence).unwrap(),
            modifications: Vec::new(),
        })
        .expect("固定容量内事件应成功入队");
    }
    let emergencySequence = u32::try_from(captureQueueCapacity).unwrap();
    let error = sink
        .append(UdpDatagramEvent {
            processId: 41_002,
            clientAddress: "192.0.2.11:53001".parse().unwrap(),
            targetAddress: "198.51.100.21:53".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: emergencySequence.to_be_bytes().to_vec(),
            capturedAtMilliseconds: u64::from(emergencySequence),
            modifications: Vec::new(),
        })
        .expect_err("第一个超限事件必须显式故障");
    assert!(error.contains("emergency"));
    assert_eq!(sink.fault(), Some(error));
    let writer = spawnSpoolWriter(Arc::clone(&spool), Arc::clone(&sink), captureReceiver);
    writer.join().expect("等待 UDP 队列满载 writer");
    for expected in 0..=emergencySequence {
        let entry = spool
            .readNext()
            .expect("读取 UDP 队列满载帧")
            .expect("UDP 队列满载帧不应丢失");
        assert_eq!(
            u32::from_be_bytes(entry.event.payload.as_slice().try_into().unwrap()),
            expected
        );
        let acknowledgement = entry.acknowledgement();
        spool
            .acknowledge(acknowledgement)
            .expect("确认 UDP 队列满载帧");
    }
    assert!(spool.readNext().expect("读取排空后的 UDP spool").is_none());
}

/// 验证同进程服务重开续接确认边界，而新控制进程代际从偏移 0 完整重放内存事务。
#[test]
fn resumesPartiallyAcknowledgedSegmentWithoutDuplicateRecording() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 游标恢复测试目录");
    let spool = UdpRecordingSpool::create(temporaryDirectory.path(), recordingGeneration)
        .expect("创建 UDP 游标恢复 spool");
    for sequence in 0..3_u32 {
        spool
            .appendEvent(&UdpDatagramEvent {
                processId: 41_003,
                clientAddress: "192.0.2.13:53003".parse().unwrap(),
                targetAddress: "198.51.100.23:443".parse().unwrap(),
                direction: UdpDatagramDirection::Up,
                payload: sequence.to_be_bytes().to_vec(),
                capturedAtMilliseconds: u64::from(sequence),
                modifications: Vec::new(),
            })
            .expect("写入 UDP 游标恢复帧");
    }
    let first = spool
        .readNext()
        .expect("读取 UDP 游标恢复首帧")
        .expect("UDP 游标恢复首帧必须存在");
    assert_eq!(first.event.payload, 0_u32.to_be_bytes());
    spool
        .acknowledge(first.acknowledgement())
        .expect("持久化 UDP 首帧确认边界");
    drop(spool);

    let reopened = UdpRecordingSpool::create(temporaryDirectory.path(), recordingGeneration)
        .expect("从确认边界重开 UDP spool");
    let second = reopened
        .readNext()
        .expect("读取 UDP 游标恢复第二帧")
        .expect("UDP 游标恢复第二帧必须存在");
    assert_eq!(second.event.payload, 1_u32.to_be_bytes());
    assert_eq!(second.sequence, 2);
    drop(reopened);

    let rebuiltProcess = UdpRecordingSpool::create(
        temporaryDirectory.path(),
        "udp-test-rebuilt-process-generation",
    )
    .expect("以新进程代际重开 UDP spool");
    let replayedFirst = rebuiltProcess
        .readNext()
        .expect("读取新进程代际首帧")
        .expect("新进程代际必须从偏移 0 完整重放");
    assert_eq!(replayedFirst.event.payload, 0_u32.to_be_bytes());
    assert_eq!(replayedFirst.sequence, 1);
}

/// 验证分段删除失败时仍保留内存节点和可用文件句柄，禁止下代从已遗忘文件重复录制。
#[test]
#[cfg(windows)]
fn retainsConsumedSegmentWhenDiskDeletionFails() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 删除失败测试目录");
    let spool = UdpRecordingSpool::create(temporaryDirectory.path(), recordingGeneration)
        .expect("创建 UDP 删除失败 spool");
    spool
        .appendEvent(&UdpDatagramEvent {
            processId: 41_004,
            clientAddress: "192.0.2.14:53004".parse().unwrap(),
            targetAddress: "198.51.100.24:53".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: b"delete-lock".to_vec(),
            capturedAtMilliseconds: 1,
            modifications: Vec::new(),
        })
        .expect("写入 UDP 删除失败帧");
    let entry = spool
        .readNext()
        .expect("读取 UDP 删除失败帧")
        .expect("UDP 删除失败帧必须存在");
    let acknowledgement = entry.acknowledgement();
    spool.close();
    let segmentPath = spoolFiles(&temporaryDirectory.path().join("capture"))
        .expect("枚举 UDP 删除失败分段")
        .into_iter()
        .next()
        .expect("UDP 删除失败分段路径必须存在");
    // Windows 共享模式刻意排除 FILE_SHARE_DELETE，真实触发 remove_file 失败而非调用测试桩。
    let deletionBlocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
        .open(&segmentPath)
        .expect("持有禁止删除共享的 UDP 分段句柄");
    let error = spool
        .acknowledge(acknowledgement)
        .expect_err("分段被外部句柄锁定时删除必须向上传播");
    assert_eq!(
        error.raw_os_error(),
        Some(32),
        "应返回 Windows 共享冲突：{error}"
    );
    assert!(
        spool.readNext().expect("删除失败后读取 spool").is_none(),
        "删除失败后内存游标必须仍停在已确认边界，不能重复返回正文"
    );
    drop(deletionBlocker);
    drop(spool);
    let reopened = UdpRecordingSpool::create(temporaryDirectory.path(), recordingGeneration)
        .expect("重开 UDP 删除失败 spool");
    assert!(!segmentPath.exists(), "重开时必须回收已完全确认的旧分段");
    drop(reopened);
}

/// 验证慢速分段删除只暂停顺序 reader，不持有全局状态锁，队尾 writer 仍可立即追加。
#[test]
fn appendsWhileConsumedSegmentDeletionIsBlocked() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 慢删除测试目录");
    let removalEntered = Arc::new((Mutex::new(false), Condvar::new()));
    let removalRelease = Arc::new((Mutex::new(false), Condvar::new()));
    let enteredForRemover = Arc::clone(&removalEntered);
    let releaseForRemover = Arc::clone(&removalRelease);
    let remover: Arc<dyn SpoolSegmentRemover> = Arc::new(move |path: &std::path::Path| {
        let (enteredLock, enteredChanged) = enteredForRemover.as_ref();
        *enteredLock.lock().expect("UDP 慢删除进入标记锁中毒") = true;
        enteredChanged.notify_one();
        let (releaseLock, releaseChanged) = releaseForRemover.as_ref();
        let mut released = releaseLock.lock().expect("UDP 慢删除释放锁中毒");
        while !*released {
            released = releaseChanged.wait(released).expect("UDP 慢删除等待锁中毒");
        }
        std::fs::remove_file(path)
    });
    let spool = UdpRecordingSpool::createWithSegmentRemover(
        temporaryDirectory.path(),
        recordingGeneration,
        remover,
    )
    .expect("创建 UDP 慢删除 spool");
    let payload = vec![0x5a; 65_535];
    let mut firstSegmentFrames = 0_u32;
    loop {
        spool
            .appendEvent(&UdpDatagramEvent {
                processId: 41_005,
                clientAddress: "192.0.2.15:53005".parse().unwrap(),
                targetAddress: "198.51.100.25:443".parse().unwrap(),
                direction: UdpDatagramDirection::Up,
                payload: payload.clone(),
                capturedAtMilliseconds: u64::from(firstSegmentFrames),
                modifications: Vec::new(),
            })
            .expect("写入 UDP 慢删除分段帧");
        if spoolFiles(&temporaryDirectory.path().join("capture"))
            .expect("枚举 UDP 慢删除分段")
            .len()
            == 2
        {
            break;
        }
        firstSegmentFrames += 1;
    }
    assert!(firstSegmentFrames > 0, "慢删除测试必须真实滚动分段");
    for _ in 1..firstSegmentFrames {
        let entry = spool
            .readNext()
            .expect("读取 UDP 慢删除前置帧")
            .expect("UDP 慢删除前置帧必须存在");
        spool
            .acknowledge(entry.acknowledgement())
            .expect("确认 UDP 慢删除前置帧");
    }
    let finalEntry = spool
        .readNext()
        .expect("读取 UDP 慢删除末帧")
        .expect("UDP 慢删除末帧必须存在");
    let spoolForAcknowledgement = Arc::clone(&spool);
    let acknowledgementThread =
        thread::spawn(move || spoolForAcknowledgement.acknowledge(finalEntry.acknowledgement()));
    let (enteredLock, enteredChanged) = removalEntered.as_ref();
    let entered = enteredChanged
        .wait_timeout_while(
            enteredLock.lock().expect("UDP 慢删除进入等待锁中毒"),
            Duration::from_secs(3),
            |entered| !*entered,
        )
        .expect("UDP 慢删除进入等待失败");
    assert!(*entered.0, "UDP 慢删除器未在期限内进入");
    let (appendSender, appendReceiver) = mpsc::channel();
    let spoolForAppend = Arc::clone(&spool);
    let appendThread = thread::spawn(move || {
        let result = spoolForAppend.appendEvent(&UdpDatagramEvent {
            processId: 41_005,
            clientAddress: "192.0.2.15:53005".parse().unwrap(),
            targetAddress: "198.51.100.25:443".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: b"writer-remains-live".to_vec(),
            capturedAtMilliseconds: 9_999,
            modifications: Vec::new(),
        });
        appendSender.send(result).expect("返回 UDP 慢删除追加结果");
    });
    appendReceiver
        .recv_timeout(Duration::from_secs(1))
        .expect("分段删除阻塞期间 append 不得等待状态锁")
        .expect("分段删除阻塞期间 append 必须成功");
    let (releaseLock, releaseChanged) = removalRelease.as_ref();
    *releaseLock.lock().expect("UDP 慢删除释放标记锁中毒") = true;
    releaseChanged.notify_one();
    appendThread.join().expect("等待 UDP 慢删除追加线程");
    acknowledgementThread
        .join()
        .expect("等待 UDP 慢删除确认线程")
        .expect("UDP 慢删除回收必须成功");
}

/// 验证最后一帧先确认、writer 后滚动时 reader 会回收旧队首并继续下一段，而不是提前退出。
#[test]
fn continuesReadingWhenWriterRotatesAfterFrontWasAcknowledged() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 延迟滚动测试目录");
    let spool = UdpRecordingSpool::create(temporaryDirectory.path(), recordingGeneration)
        .expect("创建 UDP 延迟滚动 spool");
    let payload = vec![0x6b; 65_535];
    let mut firstSegmentFrames = 0_u32;
    loop {
        spool
            .appendEvent(&UdpDatagramEvent {
                processId: 41_006,
                clientAddress: "192.0.2.16:53006".parse().unwrap(),
                targetAddress: "198.51.100.26:443".parse().unwrap(),
                direction: UdpDatagramDirection::Up,
                payload: payload.clone(),
                capturedAtMilliseconds: u64::from(firstSegmentFrames),
                modifications: Vec::new(),
            })
            .expect("写入 UDP 延迟滚动首段");
        firstSegmentFrames += 1;
        let segmentLength = std::fs::metadata(
            spoolFiles(&temporaryDirectory.path().join("capture")).expect("枚举 UDP 延迟滚动首段")
                [0]
            .clone(),
        )
        .expect("读取 UDP 延迟滚动首段长度")
        .len();
        if segmentLength > 67_045_000 {
            break;
        }
    }
    for _ in 0..firstSegmentFrames {
        let entry = spool
            .readNext()
            .expect("读取 UDP 延迟滚动首段")
            .expect("UDP 延迟滚动首段帧必须存在");
        spool
            .acknowledge(entry.acknowledgement())
            .expect("确认 UDP 延迟滚动首段");
    }
    let (readSender, readReceiver) = mpsc::channel();
    let readerSpool = Arc::clone(&spool);
    let reader = thread::spawn(move || {
        readSender
            .send(readerSpool.readNext())
            .expect("返回 UDP 延迟滚动读取结果");
    });
    spool
        .appendEvent(&UdpDatagramEvent {
            processId: 41_006,
            clientAddress: "192.0.2.16:53006".parse().unwrap(),
            targetAddress: "198.51.100.26:443".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: payload.clone(),
            capturedAtMilliseconds: 99_999,
            modifications: Vec::new(),
        })
        .expect("触发 UDP 延迟滚动");
    let next = readReceiver
        .recv_timeout(Duration::from_secs(3))
        .expect("writer 滚动后 reader 必须继续")
        .expect("读取 UDP 延迟滚动次段")
        .expect("UDP 延迟滚动次段帧必须存在");
    assert_eq!(next.event.payload, payload);
    reader.join().expect("等待 UDP 延迟滚动 reader");
}

/// 验证 clear 水位与 UDP 三步事务串行：旧积压只确认，新 epoch 正文保持可见且不会永久 fault。
#[tokio::test]
async fn clearSkipsOldBacklogAndRecordsNewEpoch() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP clear epoch 测试目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建 UDP clear epoch 录制会话");
    let coordination =
        UdpRecordingCoordination::load(temporaryDirectory.path(), "udp-clear-process-generation")
            .expect("加载 UDP clear epoch 协调器");
    let recordingLock = Arc::new(tokio::sync::Mutex::new(()));
    let runtime = startCoordinatedUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        Arc::clone(&coordination),
        Arc::clone(&recordingLock),
    )
    .expect("启动 UDP clear epoch 代际");
    let sink = runtime.sink();
    let clearGuard = recordingLock.lock().await;
    sink.append(UdpDatagramEvent {
        processId: 41_007,
        clientAddress: "192.0.2.17:53007".parse().unwrap(),
        targetAddress: "198.51.100.27:53".parse().unwrap(),
        direction: UdpDatagramDirection::Up,
        payload: b"old-before-clear".to_vec(),
        capturedAtMilliseconds: 1,
        modifications: Vec::new(),
    })
    .expect("加入 UDP clear 前积压");
    coordination
        .advanceAndPersist(temporaryDirectory.path())
        .expect("持久推进 UDP clear 水位");
    recording.clearSession().await.expect("清空 UDP 录制会话");
    drop(clearGuard);
    sink.append(UdpDatagramEvent {
        processId: 41_007,
        clientAddress: "192.0.2.17:53007".parse().unwrap(),
        targetAddress: "198.51.100.27:53".parse().unwrap(),
        direction: UdpDatagramDirection::Up,
        payload: b"new-after-clear".to_vec(),
        capturedAtMilliseconds: 2,
        modifications: Vec::new(),
    })
    .expect("加入 UDP clear 后正文");
    runtime
        .stopAndDrain()
        .await
        .expect("排空 UDP clear epoch 代际");
    let metadata = recording
        .listMetadata()
        .await
        .expect("读取 UDP clear 后事务");
    assert_eq!(metadata.len(), 1);
    assert_eq!(
        recording
            .getBody(&metadata[0].transactionId, MessageSide::Request)
            .await
            .expect("读取 UDP clear 后正文")
            .bytes,
        b"new-after-clear"
    );
}

/// 验证 clear 水位跨控制进程持久：新 RecordingSession 重放旧 spool 时只确认，不重建已清空正文。
#[tokio::test]
async fn durableClearBarrierSkipsOldProcessBacklog() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 跨进程 clear 测试目录");
    let oldSpool = UdpRecordingSpool::create(temporaryDirectory.path(), "udp-old-process")
        .expect("创建旧进程 UDP spool");
    oldSpool
        .appendEvent(&UdpDatagramEvent {
            processId: 41_009,
            clientAddress: "192.0.2.19:53009".parse().unwrap(),
            targetAddress: "198.51.100.29:53".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: b"cleared-before-crash".to_vec(),
            capturedAtMilliseconds: 1,
            modifications: Vec::new(),
        })
        .expect("写入 clear 前旧进程积压");
    oldSpool.close();
    drop(oldSpool);
    let oldCoordination =
        UdpRecordingCoordination::load(temporaryDirectory.path(), "udp-old-process")
            .expect("加载旧进程 clear 协调器");
    oldCoordination
        .advanceAndPersist(temporaryDirectory.path())
        .expect("持久化旧进程 clear 水位");

    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording-new-process"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建新控制进程 RecordingSession");
    let newCoordination =
        UdpRecordingCoordination::load(temporaryDirectory.path(), "udp-new-process")
            .expect("加载新控制进程协调器");
    let runtime = startCoordinatedUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        newCoordination,
        Arc::new(tokio::sync::Mutex::new(())),
    )
    .expect("启动新控制进程 UDP 代际");
    runtime
        .stopAndDrain()
        .await
        .expect("排空跨进程 clear 后旧积压");
    assert!(
        recording
            .listMetadata()
            .await
            .expect("读取跨进程 clear 后事务")
            .is_empty()
    );
}

/// 验证事务已提交但确认游标替换失败时，同进程服务重启只重试 ACK，不生成重复事务。
#[cfg(windows)]
#[tokio::test]
async fn retriesAcknowledgementAfterCommitWithoutDuplicateTransaction() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP ACK 重试测试目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建 UDP ACK 重试录制会话");
    let coordination = UdpRecordingCoordination::load(temporaryDirectory.path(), "udp-ack-process")
        .expect("加载 UDP ACK 重试协调器");
    let recordingLock = Arc::new(tokio::sync::Mutex::new(()));
    let firstRuntime = startCoordinatedUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        Arc::clone(&coordination),
        Arc::clone(&recordingLock),
    )
    .expect("启动首个 UDP ACK 重试代际");
    let cursorPath = temporaryDirectory
        .path()
        .join("capture")
        .join("udpCapture-00000000000000000001.ack.next");
    let cursorBlocker = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&cursorPath)
        .expect("持有禁止替换的 UDP ACK 游标");
    let sink = firstRuntime.sink();
    sink.append(UdpDatagramEvent {
        processId: 41_010,
        clientAddress: "192.0.2.20:53010".parse().unwrap(),
        targetAddress: "198.51.100.30:53".parse().unwrap(),
        direction: UdpDatagramDirection::Up,
        payload: b"commit-before-ack".to_vec(),
        capturedAtMilliseconds: 1,
        modifications: Vec::new(),
    })
    .expect("写入 UDP ACK 重试正文");
    let deadline = Instant::now() + Duration::from_secs(3);
    while recording
        .listMetadata()
        .await
        .expect("等待 ACK 失败前事务提交")
        .is_empty()
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        recording
            .listMetadata()
            .await
            .expect("确认 ACK 失败前事务已提交")
            .len(),
        1
    );
    assert!(firstRuntime.stopAndDrain().await.is_err());
    drop(cursorBlocker);

    let secondRuntime = startCoordinatedUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        coordination,
        recordingLock,
    )
    .expect("重启同进程 UDP ACK 代际");
    secondRuntime
        .stopAndDrain()
        .await
        .expect("重试事务确认并排空");
    assert_eq!(
        recording
            .listMetadata()
            .await
            .expect("读取 UDP ACK 重试事务")
            .len(),
        1
    );
}

/// 验证服务重启创建全新 sink；上一代正常排空后，下一代不会继承 sender 关闭或 fault 状态。
#[tokio::test]
async fn recreatesHealthyPipelineForEveryServiceGeneration() {
    let temporaryDirectory = tempfile::tempdir().expect("创建 UDP 代际重启测试目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建 UDP 代际重启录制会话");
    for generation in 1..=2_u32 {
        let runtime = startUdpRecordingGeneration(
            temporaryDirectory.path(),
            recording.clone(),
            emptyProcessNameResolver(),
            recordingGeneration,
        )
        .expect("启动 UDP 录制代际");
        let sink = runtime.sink();
        sink.append(UdpDatagramEvent {
            processId: 42_000 + generation,
            clientAddress: "192.0.2.12:53002".parse().unwrap(),
            targetAddress: "198.51.100.22:443".parse().unwrap(),
            direction: UdpDatagramDirection::Up,
            payload: generation.to_be_bytes().to_vec(),
            capturedAtMilliseconds: u64::from(generation),
            modifications: Vec::new(),
        })
        .expect("新 UDP 代际必须接受首包");
        runtime.stopAndDrain().await.expect("排空 UDP 录制代际");
        assert!(sink.fault().is_none());
    }
    assert_eq!(
        recording
            .listMetadata()
            .await
            .expect("读取 UDP 代际事务")
            .len(),
        2
    );
}

/// 仅由真实驱动父测试启动；逐包确认使网络丢包、乱序或正文损坏立即成为子进程失败。
#[test]
#[ignore = "仅由管理员真实 WinDivert spool 测试启动"]
fn udpSpoolRealDriverHelper() {
    if std::env::var_os(realDriverHelperEnvironment).is_none() {
        return;
    }
    let target: SocketAddr = std::env::var(realDriverTargetEnvironment)
        .expect("缺少真实 UDP spool 目标")
        .parse()
        .expect("真实 UDP spool 目标格式错误");
    let socket = UdpSocket::bind("0.0.0.0:0").expect("绑定真实 UDP spool 子进程套接字");
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("设置真实 UDP spool 子进程超时");
    std::io::stdout()
        .write_all(b"R")
        .expect("通知父测试 UDP socket 已完成 BIND");
    std::io::stdout().flush().expect("刷新 UDP BIND ready 信号");
    let mut trigger = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut trigger)
        .expect("等待真实 UDP spool 启动信号");
    let dnsTarget: SocketAddr = std::env::var(realDriverDnsTargetEnvironment)
        .expect("缺少真实 DNS 目标")
        .parse()
        .expect("真实 DNS 目标格式错误");
    let transactionId = (std::process::id() as u16).to_be_bytes();
    let mut dnsQuery = vec![
        transactionId[0],
        transactionId[1],
        0x01,
        0x00,
        0x00,
        0x01,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ];
    dnsQuery.extend_from_slice(&[
        5, b's', b'p', b'r', b'a', b'k', 7, b'i', b'n', b'v', b'a', b'l', b'i', b'd', 0,
    ]);
    dnsQuery.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
    socket
        .send_to(&dnsQuery, dnsTarget)
        .expect("发送选中进程真实 DNS 查询");
    let mut dnsResponse = [0_u8; 1_500];
    let (dnsBytes, dnsSource) = socket
        .recv_from(&mut dnsResponse)
        .expect("接收选中进程真实 DNS 响应");
    assert_eq!(dnsSource, dnsTarget);
    assert!(dnsBytes >= 12);
    assert_eq!(&dnsResponse[..2], &transactionId);
    for sequence in 0..stressDatagrams {
        let payload = sequence.to_be_bytes();
        socket
            .send_to(&payload, target)
            .expect("发送真实 UDP 数据报");
        let mut response = [0_u8; 4];
        let (byteCount, source) = socket
            .recv_from(&mut response)
            .expect("接收真实 UDP 数据报响应");
        assert_eq!(source, target);
        assert_eq!(byteCount, payload.len());
        assert_eq!(response, payload);
    }
}

/// 让真实选中进程的双向数据报完整经过 SNIFF、固定队列、磁盘 spool 和 RecordingSession。
#[tokio::test]
#[ignore = "需要管理员权限、真实 WinDivert 驱动和可用 DNS"]
async fn recordsRealDriverUdpBurstThroughCompleteSpoolPipeline() {
    let routeProbe = UdpSocket::bind("0.0.0.0:0").expect("创建真实 UDP spool 路由探针");
    routeProbe
        .connect("1.1.1.1:53")
        .expect("系统缺少可用 IPv4 UDP 路由");
    let lanAddress = routeProbe.local_addr().expect("读取本机 LAN 地址").ip();
    let echoSocket =
        UdpSocket::bind(SocketAddr::new(lanAddress, 0)).expect("绑定真实 UDP spool 回显端口");
    echoSocket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("设置真实 UDP spool 回显超时");
    let targetAddress = echoSocket
        .local_addr()
        .expect("读取真实 UDP spool 回显地址");
    let dnsTarget = systemIpv4DnsTarget();
    let proxyPort = TcpListener::bind("127.0.0.1:0")
        .expect("保留真实 UDP spool 捕获配置端口")
        .local_addr()
        .expect("读取真实 UDP spool 捕获配置端口")
        .port();
    let temporaryDirectory = tempfile::tempdir().expect("创建真实 UDP spool 临时目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().join("recording"),
        memoryBodyThreshold: 1,
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建真实 UDP spool 录制会话");
    let runtime = startUdpRecordingGeneration(
        temporaryDirectory.path(),
        recording.clone(),
        emptyProcessNameResolver(),
        recordingGeneration,
    )
    .expect("启动真实 UDP spool 完整代际");
    let sink = runtime.sink();
    let capture = ProcessCapture::new();
    capture.setUdpDatagramSink(Some(sink.clone()));
    capture
        .start(ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort,
            proxyAddress: "0.0.0.0".parse().unwrap(),
        })
        .expect("启动真实 UDP spool WinDivert 捕获器");
    let mut child = Command::new(std::env::current_exe().expect("读取当前测试程序路径"))
        .args([
            "--ignored",
            "--exact",
            "udpSpoolRealDriverHelper",
            "--nocapture",
        ])
        .env(realDriverHelperEnvironment, "1")
        .env(realDriverTargetEnvironment, targetAddress.to_string())
        .env(realDriverDnsTargetEnvironment, dnsTarget.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动真实 UDP spool 子进程");
    waitForReadySignal(child.stdout.as_mut().expect("真实 UDP spool 子进程 stdout"));
    capture
        .updateProcessIds(BTreeSet::from([child.id()]))
        .expect("热加入真实 UDP spool 子进程");
    child
        .stdin
        .as_mut()
        .expect("真实 UDP spool 子进程 stdin")
        .write_all(&[1])
        .expect("触发真实 UDP spool 子进程");
    for expectedSequence in 0..stressDatagrams {
        let mut request = [0_u8; 4];
        let (byteCount, clientAddress) = echoSocket
            .recv_from(&mut request)
            .expect("接收真实 UDP spool 请求");
        assert_eq!(byteCount, request.len());
        assert_eq!(u32::from_be_bytes(request), expectedSequence);
        echoSocket
            .send_to(&request, clientAddress)
            .expect("发送真实 UDP spool 响应");
        if expectedSequence == stressDatagrams / 2 {
            assert!(
                ("music.163.com", 443)
                    .to_socket_addrs()
                    .expect("真实 UDP spool 高负载期间 DNS 解析失败")
                    .next()
                    .is_some()
            );
        }
    }
    let childOutput = child.wait_with_output().expect("等待真实 UDP spool 子进程");
    assert!(
        childOutput.status.success(),
        "真实 UDP spool 子进程失败：{childOutput:?}"
    );
    let expectedEntries = u64::from(stressDatagrams) * 2 + 2;
    let deadline = Instant::now() + Duration::from_secs(10);
    while capture.snapshot().redirectedPackets + capture.snapshot().restoredPackets
        < expectedEntries
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    let snapshot = capture.snapshot();
    assert_eq!(
        snapshot.redirectedPackets + snapshot.restoredPackets,
        expectedEntries,
        "真实 UDP 捕获计数不完整：{snapshot:?}"
    );
    assert!(
        snapshot.lastError.is_none(),
        "真实 UDP spool 捕获故障：{snapshot:?}"
    );
    capture.stop().expect("停止真实 UDP spool WinDivert 捕获器");
    capture.setUdpDatagramSink(None);
    runtime
        .stopAndDrain()
        .await
        .expect("停止并排空真实 UDP spool 完整代际");
    let metadata = recording
        .listMetadata()
        .await
        .expect("读取真实 UDP spool 事务");
    assert_eq!(metadata.len(), usize::try_from(expectedEntries).unwrap());
    assert!(
        metadata
            .iter()
            .filter(|entry| entry.urlDisplay == format!("udp://{dnsTarget}"))
            .count()
            >= 2,
        "选中子进程 DNS 请求/响应没有完整进入录制：{metadata:?}"
    );
    assert!(
        spoolFiles(&temporaryDirectory.path().join("capture"))
            .expect("枚举真实 UDP spool 分段")
            .is_empty()
    );
}
