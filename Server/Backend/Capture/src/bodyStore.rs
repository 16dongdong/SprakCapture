use std::{
    collections::HashSet,
    fmt,
    fs::OpenOptions,
    io::ErrorKind,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use crate::{BodyHandleMeta, CaptureError, MessageSide};

/// 公开正文所在介质类别，便于诊断预算行为，同时隐藏实际文件路径。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyStorageKind {
    Memory,
    Spill,
}

#[derive(Clone, Debug)]
enum BodyStorage {
    Memory(Arc<[u8]>),
    Spill(PathBuf),
}

/// 持有一份完整正文的元信息与内部存储引用；列表模型不会包含该类型。
#[derive(Clone, Debug)]
pub struct BodyRef {
    meta: BodyHandleMeta,
    storage: BodyStorage,
}

/// 封装租约实际读取介质；内存与 spill 共享同一只读偏移语义。
#[derive(Clone)]
enum BodyReadStorage {
    Memory(Arc<[u8]>),
    Spill(Arc<AsyncMutex<fs::File>>),
}

/// 持有正文的稳定只读租约；事务淘汰或 clear 后仍可把已声明长度完整发送给活动响应。
///
/// 内存正文共享原始 Arc；spill 正文在租约建立时打开一次文件句柄，后续只在该句柄上 seek/read。
/// Windows 标准只读打开允许 FILE_SHARE_DELETE，因此路径可立即清理，实际文件空间在最后一个
/// 活动句柄释放后由系统回收。该类型不暴露路径，也不允许写入录制正文。
#[derive(Clone)]
pub struct BodyReadLease {
    meta: BodyHandleMeta,
    storage: BodyReadStorage,
}

impl fmt::Debug for BodyReadLease {
    /// 调试输出只包含非秘密元信息与介质类别，禁止泄露 spill 的内部文件路径。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BodyReadLease")
            .field("meta", &self.meta)
            .field("storageKind", &self.storageKind())
            .finish()
    }
}

impl BodyReadLease {
    /// 返回租约建立时的正文元信息；该快照在事务淘汰后仍保持不变。
    pub fn meta(&self) -> &BodyHandleMeta {
        &self.meta
    }

    /// 返回稳定租约使用的介质类别，不暴露内存地址、句柄或 spill 路径。
    pub const fn storageKind(&self) -> BodyStorageKind {
        match &self.storage {
            BodyReadStorage::Memory(_) => BodyStorageKind::Memory,
            BodyReadStorage::Spill(_) => BodyStorageKind::Spill,
        }
    }

    /// 从稳定租约按偏移读取有界正文；事务表、FIFO 与 clear 生命周期不参与后续读取。
    ///
    /// 内存租约只复制请求区间；spill 租约串行化共享文件游标后执行 seek/read_exact。
    /// 偏移越界、算术错误或句柄 I/O 失败返回精确 CaptureError，绝不返回伪造短块。
    pub async fn readRange(
        &self,
        offset: usize,
        maximumBytes: usize,
    ) -> Result<Vec<u8>, CaptureError> {
        if offset > self.meta.storedBytes {
            return Err(CaptureError::io(std::io::Error::new(
                ErrorKind::InvalidInput,
                "captureBodyRangeOffsetOutOfBounds",
            )));
        }
        let readBytes = maximumBytes.min(self.meta.storedBytes - offset);
        match &self.storage {
            BodyReadStorage::Memory(bytes) => Ok(bytes[offset..offset + readBytes].to_vec()),
            BodyReadStorage::Spill(file) => {
                let mut file = file.lock().await;
                file.seek(std::io::SeekFrom::Start(offset as u64))
                    .await
                    .map_err(CaptureError::io)?;
                let mut bytes = vec![0_u8; readBytes];
                file.read_exact(&mut bytes)
                    .await
                    .map_err(CaptureError::io)?;
                Ok(bytes)
            }
        }
    }
}

/// 持有尚未登记进 RecordingSession 的正文；任务取消或提前返回时 Drop 会移除已落盘文件。
pub(crate) struct StagedBody {
    bodyReference: Option<BodyRef>,
    spillGuard: Option<SpillFileGuard>,
}

/// 持有一侧流式正文的临时落盘文件；数据面按块追加，终态时再原子改名并绑定到事务。
///
/// 运行上下文：长连接不能把完整正文积压在内存中，因此该对象从创建开始就使用会话 spill 目录。
/// 失败语义：追加、同步或改名失败均返回结构化 I/O 错误；未提交对象析构时由文件守卫回收。
pub struct BodySpool {
    file: Option<fs::File>,
    pendingPath: PathBuf,
    committedPath: PathBuf,
    writtenBytes: usize,
    spillGuard: SpillFileGuard,
}

impl BodySpool {
    /// 把 `bytes` 完整追加到当前方向的 spool；调用方必须用有界背压等待本调用，禁止丢弃或仅保留前缀。
    /// 文件已经进入提交阶段或底层写入失败时返回结构化 I/O 错误，计数只包含 `write_all` 成功的字节。
    pub async fn append(&mut self, bytes: &[u8]) -> Result<(), CaptureError> {
        let file = self.file.as_mut().ok_or_else(|| {
            CaptureError::io(std::io::Error::other("captureBodySpoolAlreadyFinished"))
        })?;
        file.write_all(bytes).await.map_err(CaptureError::io)?;
        self.writtenBytes = self.writtenBytes.saturating_add(bytes.len());
        Ok(())
    }

    /// 返回已经由文件写入确认的正文长度；该查询不执行 I/O，因而没有失败分支。
    pub const fn writtenBytes(&self) -> usize {
        self.writtenBytes
    }

    /// 使用 `meta` 同步并原子提交临时文件；flush、sync 或 rename 失败时返回结构化 I/O 错误。
    async fn stage(mut self, meta: BodyHandleMeta) -> Result<StagedBody, CaptureError> {
        let mut file = self.file.take().ok_or_else(|| {
            CaptureError::io(std::io::Error::other("captureBodySpoolAlreadyFinished"))
        })?;
        file.flush().await.map_err(CaptureError::io)?;
        file.sync_all().await.map_err(CaptureError::io)?;
        drop(file);
        fs::rename(&self.pendingPath, &self.committedPath)
            .await
            .map_err(CaptureError::io)?;
        self.spillGuard.moveTo(self.committedPath.clone());
        Ok(StagedBody {
            bodyReference: Some(BodyRef {
                meta,
                storage: BodyStorage::Spill(self.committedPath.clone()),
            }),
            spillGuard: Some(self.spillGuard),
        })
    }
}

impl StagedBody {
    /// 在会话状态锁内完成唯一登记后解除文件守卫；调用后不再触发自动清理。
    pub(crate) fn commit(mut self) -> BodyRef {
        if let Some(spillGuard) = self.spillGuard.as_mut() {
            spillGuard.disarm();
        }
        self.bodyReference
            .take()
            .expect("stagedBodyMissingReference")
    }
}

/// 在阻塞文件任务和异步调用方之间传递清理所有权，覆盖写入、rename 和登记前取消窗口。
struct SpillFileGuard {
    filePath: Option<PathBuf>,
    orphanedSpills: Arc<Mutex<HashSet<PathBuf>>>,
}

impl SpillFileGuard {
    /// 创建指向活动文件的守卫；活动文件仍由写入任务持有，不能提前登记到孤儿清理队列。
    ///
    /// 运行上下文：spool 和一次性 spill 从创建到权威登记前都使用该守卫；只有析构删除失败才变成孤儿。
    /// 失败语义：构造只转移路径所有权，没有 I/O；后续 Drop 无法删除时才登记可重试路径。
    fn new(filePath: PathBuf, orphanedSpills: Arc<Mutex<HashSet<PathBuf>>>) -> Self {
        Self {
            filePath: Some(filePath),
            orphanedSpills,
        }
    }

    /// 原子 rename 成功后更新守卫目标；活动文件仍不进入孤儿集合，避免并发事务清理正在录制的正文。
    ///
    /// 运行上下文：文件已经从临时名切换到最终名，但 BodyRef 尚未写入会话权威状态。
    /// 失败语义：纯内存路径替换没有失败分支；若随后取消，Drop 负责删除最终路径。
    fn moveTo(&mut self, filePath: PathBuf) {
        self.filePath = Some(filePath);
    }

    /// 仅允许权威状态完成登记后解除守卫；已被 BodyRef 持有的文件不属于孤儿清理范围。
    ///
    /// 运行上下文：调用点必须位于 RecordingSession 写锁内并已经建立可达 BodyRef。
    /// 失败语义：纯内存所有权转换没有失败分支，解除后 Drop 不再删除正文。
    fn disarm(&mut self) {
        self.filePath = None;
    }
}

impl Drop for SpillFileGuard {
    /// 取消路径先同步回滚；只有删除失败的不可达文件才进入孤儿集合并由显式 cleanup 重试。
    ///
    /// 运行上下文：任务取消、提交失败或未解除守卫时由 Rust 自动调用；已提交 BodyRef 的守卫没有路径。
    /// 失败语义：NotFound 视为已回收，其它删除错误登记路径但不在析构阶段 panic。
    fn drop(&mut self) {
        let Some(filePath) = self.filePath.take() else {
            return;
        };
        match std::fs::remove_file(&filePath) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => {
                lockOrphanedSpills(&self.orphanedSpills).insert(filePath);
            }
        }
    }
}

/// 获取不可达 spill 注册表；若其它线程 panic 导致中毒，仍保留集合以便继续资源回收。
fn lockOrphanedSpills(
    orphanedSpills: &Mutex<HashSet<PathBuf>>,
) -> MutexGuard<'_, HashSet<PathBuf>> {
    orphanedSpills
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

impl BodyRef {
    /// 返回不含文件路径的正文元信息，调用方可安全放入详情响应。
    pub fn meta(&self) -> &BodyHandleMeta {
        &self.meta
    }

    /// 返回正文所在介质，供资源观测和测试验证 spill 边界。
    pub const fn storageKind(&self) -> BodyStorageKind {
        match &self.storage {
            BodyStorage::Memory(_) => BodyStorageKind::Memory,
            BodyStorage::Spill(_) => BodyStorageKind::Spill,
        }
    }

    /// 计算正文引用在固定结构之外持有的元数据容量；正文实际字节由独立正文预算计费。
    pub(crate) fn metadataStorageBytes(&self) -> usize {
        let textBytes = self
            .meta
            .transactionId
            .capacity()
            .saturating_add(self.meta.contentType.capacity())
            .saturating_add(self.meta.encoding.capacity());
        match &self.storage {
            BodyStorage::Memory(_) => textBytes,
            BodyStorage::Spill(path) => textBytes.saturating_add(path.capacity()),
        }
    }
}

/// 管理单一 RecordingSession 的正文文件；调用方负责用会话状态锁串行化 clear/store。
pub(crate) struct BodyStore {
    sessionDirectory: PathBuf,
    memoryThreshold: usize,
    orphanedSpills: Arc<Mutex<HashSet<PathBuf>>>,
}

impl BodyStore {
    /// 创建会话专属 spill 目录；目录不可创建时返回结构化 I/O 错误。
    pub(crate) async fn new(
        rootDirectory: &Path,
        recordingSessionId: &str,
        memoryThreshold: usize,
    ) -> Result<Self, CaptureError> {
        let sessionDirectory = rootDirectory.join(format!("recording-{recordingSessionId}"));
        fs::create_dir_all(&sessionDirectory)
            .await
            .map_err(CaptureError::io)?;
        Ok(Self {
            sessionDirectory,
            memoryThreshold,
            orphanedSpills: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// 暂存完整正文；超过内存阈值时 spill 由阻塞任务原子 rename，登记前取消会由守卫删除文件。
    pub(crate) async fn store(
        &self,
        transactionId: &str,
        side: MessageSide,
        bytes: &[u8],
        meta: BodyHandleMeta,
    ) -> Result<StagedBody, CaptureError> {
        if bytes.len() <= self.memoryThreshold {
            return Ok(StagedBody {
                bodyReference: Some(BodyRef {
                    meta,
                    storage: BodyStorage::Memory(Arc::from(bytes)),
                }),
                spillGuard: None,
            });
        }
        let uniqueSuffix = Uuid::new_v4();
        let fileName = format!(
            "{}-{}-{}.body",
            side.fileLabel(),
            transactionId,
            uniqueSuffix
        );
        let filePath = self.sessionDirectory.join(fileName);
        let pendingPath = self
            .sessionDirectory
            .join(format!(".pending-{uniqueSuffix}.body"));
        let ownedBytes = bytes.to_vec();
        let committedFilePath = filePath.clone();
        let orphanedSpills = Arc::clone(&self.orphanedSpills);
        let spillGuard = tokio::task::spawn_blocking(move || {
            Self::writeSpillAtomically(
                &pendingPath,
                &committedFilePath,
                &ownedBytes,
                orphanedSpills,
            )
        })
        .await
        .map_err(|error| {
            CaptureError::io(std::io::Error::other(format!(
                "captureBodyWriteTaskFailed:{error}"
            )))
        })?
        .map_err(CaptureError::io)?;
        Ok(StagedBody {
            bodyReference: Some(BodyRef {
                meta,
                storage: BodyStorage::Spill(filePath),
            }),
            spillGuard: Some(spillGuard),
        })
    }

    /// 为 `transactionId` 的 `side` 创建增量正文 spool；目录或文件创建失败时返回结构化 I/O 错误。
    pub(crate) async fn createSpool(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodySpool, CaptureError> {
        let uniqueSuffix = Uuid::new_v4();
        let committedPath = self.sessionDirectory.join(format!(
            "{}-{}-{}.body",
            side.fileLabel(),
            transactionId,
            uniqueSuffix
        ));
        let pendingPath = self
            .sessionDirectory
            .join(format!(".stream-{uniqueSuffix}.body"));
        let spillGuard = SpillFileGuard::new(pendingPath.clone(), Arc::clone(&self.orphanedSpills));
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&pendingPath)
            .await
            .map_err(CaptureError::io)?;
        Ok(BodySpool {
            file: Some(file),
            pendingPath,
            committedPath,
            writtenBytes: 0,
            spillGuard,
        })
    }

    /// 使用 `meta` 把写完的 `spool` 转换为待登记正文；同步或原子改名失败时返回结构化 I/O 错误。
    pub(crate) async fn stageSpool(
        &self,
        spool: BodySpool,
        meta: BodyHandleMeta,
    ) -> Result<StagedBody, CaptureError> {
        spool.stage(meta).await
    }

    /// 按 create-new、完整写入、同步、rename 顺序提交 spill，任何中途失败都由守卫回收当前路径。
    fn writeSpillAtomically(
        pendingPath: &Path,
        filePath: &Path,
        bytes: &[u8],
        orphanedSpills: Arc<Mutex<HashSet<PathBuf>>>,
    ) -> Result<SpillFileGuard, std::io::Error> {
        let mut spillGuard = SpillFileGuard::new(pendingPath.to_path_buf(), orphanedSpills);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(pendingPath)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(pendingPath, filePath)?;
        spillGuard.moveTo(filePath.to_path_buf());
        Ok(spillGuard)
    }

    /// 按引用读取正文；spill 文件被外部删除时返回 BodyNotFound，其它错误保留 I/O 来源。
    pub(crate) async fn read(&self, bodyReference: &BodyRef) -> Result<Vec<u8>, CaptureError> {
        match &bodyReference.storage {
            BodyStorage::Memory(bytes) => Ok(bytes.to_vec()),
            BodyStorage::Spill(filePath) => {
                let bytes = match fs::read(filePath).await {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Err(CaptureError::BodyNotFound);
                    }
                    Err(error) => return Err(CaptureError::io(error)),
                };
                if bytes.len() != bodyReference.meta.storedBytes {
                    return Err(CaptureError::io(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "captureBodyLengthMismatch",
                    )));
                }
                Ok(bytes)
            }
        }
    }

    /// 按偏移读取正文的有界分块，供流式控制响应在背压下逐块发送大型正文。
    ///
    /// 运行上下文：内存正文只复制请求区间，spill 正文通过异步 seek/read_exact 读取，任何时刻
    /// 都不分配超过 `maximumBytes` 的缓冲区。`offset` 可以等于正文长度并返回空块。
    /// 失败语义：偏移越界、文件长度与已登记元信息不一致或底层 I/O 失败时返回精确错误，
    /// 调用方必须终止当前响应，禁止用短块伪装完整正文。
    pub(crate) async fn readRange(
        &self,
        bodyReference: &BodyRef,
        offset: usize,
        maximumBytes: usize,
    ) -> Result<Vec<u8>, CaptureError> {
        let storedBytes = bodyReference.meta.storedBytes;
        if offset > storedBytes {
            return Err(CaptureError::io(std::io::Error::new(
                ErrorKind::InvalidInput,
                "captureBodyRangeOffsetOutOfBounds",
            )));
        }
        let readBytes = maximumBytes.min(storedBytes - offset);
        match &bodyReference.storage {
            BodyStorage::Memory(bytes) => Ok(bytes[offset..offset + readBytes].to_vec()),
            BodyStorage::Spill(filePath) => {
                let mut file = match fs::File::open(filePath).await {
                    Ok(file) => file,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Err(CaptureError::BodyNotFound);
                    }
                    Err(error) => return Err(CaptureError::io(error)),
                };
                let fileBytes = file.metadata().await.map_err(CaptureError::io)?.len();
                if fileBytes != storedBytes as u64 {
                    return Err(CaptureError::io(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "captureBodyLengthMismatch",
                    )));
                }
                file.seek(std::io::SeekFrom::Start(offset as u64))
                    .await
                    .map_err(CaptureError::io)?;
                let mut bytes = vec![0_u8; readBytes];
                file.read_exact(&mut bytes)
                    .await
                    .map_err(CaptureError::io)?;
                Ok(bytes)
            }
        }
    }

    /// 在正文仍受会话读锁保护时建立稳定租约；spill 文件会在释放状态锁前完成打开与长度校验。
    ///
    /// 调用方必须先从当前事务克隆或借用 BodyRef。文件不存在、长度变化或打开失败返回错误，
    /// 不创建半有效租约。成功后 clear/淘汰可删除路径但不会关闭租约持有的独立读句柄。
    pub(crate) async fn lease(
        &self,
        bodyReference: &BodyRef,
    ) -> Result<BodyReadLease, CaptureError> {
        let storage = match &bodyReference.storage {
            BodyStorage::Memory(bytes) => BodyReadStorage::Memory(Arc::clone(bytes)),
            BodyStorage::Spill(filePath) => {
                let file = match fs::File::open(filePath).await {
                    Ok(file) => file,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Err(CaptureError::BodyNotFound);
                    }
                    Err(error) => return Err(CaptureError::io(error)),
                };
                let fileBytes = file.metadata().await.map_err(CaptureError::io)?.len();
                if fileBytes != bodyReference.meta.storedBytes as u64 {
                    return Err(CaptureError::io(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "captureBodyLengthMismatch",
                    )));
                }
                BodyReadStorage::Spill(Arc::new(AsyncMutex::new(file)))
            }
        };
        Ok(BodyReadLease {
            meta: bodyReference.meta.clone(),
            storage,
        })
    }

    /// 删除单个 spill；内存引用无需操作，重复删除保持资源回收幂等。
    pub(crate) async fn remove(&self, bodyReference: &BodyRef) -> Result<(), CaptureError> {
        let BodyStorage::Spill(filePath) = &bodyReference.storage else {
            return Ok(());
        };
        match fs::remove_file(filePath).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CaptureError::io(error)),
        }
    }

    /// 重试取消守卫未能同步删除的不可达 spill；活动 spool 从不进入该集合，因此不会被并发清理误删。
    ///
    /// 运行上下文：RecordingSession 在持有状态写锁并完成正文淘汰后调用，集合只包含已经失去业务所有者的路径。
    /// 失败语义：NotFound 视为幂等成功；其它 I/O 错误保留当前路径并返回，下一次状态变更继续重试。
    pub(crate) async fn cleanupOrphanedSpills(&self) -> Result<(), CaptureError> {
        loop {
            let filePath = lockOrphanedSpills(&self.orphanedSpills)
                .iter()
                .next()
                .cloned();
            let Some(filePath) = filePath else {
                return Ok(());
            };
            match fs::remove_file(&filePath).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(CaptureError::io(error)),
            }
            lockOrphanedSpills(&self.orphanedSpills).remove(&filePath);
        }
    }

    /// 返回尚未完成回收的不可达 spill 数量，供录制快照暴露清理健康状态。
    pub(crate) fn pendingOrphanCount(&self) -> usize {
        lockOrphanedSpills(&self.orphanedSpills).len()
    }

    /// 清空并重建当前会话目录，使正文不能再通过事务表或原始 spill 路径建立新读取。
    ///
    /// 已建立的 BodyReadLease 持有独立打开句柄，仍可把活动 HTTP 响应读完；路径在 clear
    /// 返回前删除，文件空间由操作系统在最后一个租约释放后回收。该生命周期边界不能改成
    /// 主动关闭活动租约，否则已经声明 Content-Length 的媒体响应会在中途被截断。
    pub(crate) async fn clear(&self) -> Result<(), CaptureError> {
        match fs::remove_dir_all(&self.sessionDirectory).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(CaptureError::io(error)),
        }
        fs::create_dir_all(&self.sessionDirectory)
            .await
            .map_err(CaptureError::io)?;
        lockOrphanedSpills(&self.orphanedSpills).clear();
        Ok(())
    }

    /// 关闭录制会话并删除专属目录；成功后该 BodyStore 不得再参与写入或建立新租约。
    ///
    /// 已建立租约与 clear 使用相同的延迟回收语义，允许关闭前已经发出的响应自然完成。
    pub(crate) async fn close(&self) -> Result<(), CaptureError> {
        match fs::remove_dir_all(&self.sessionDirectory).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(CaptureError::io(error)),
        }
        lockOrphanedSpills(&self.orphanedSpills).clear();
        Ok(())
    }
}
