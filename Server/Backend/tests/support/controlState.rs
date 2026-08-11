use std::{ops::Deref, path::Path};

use proxy_backend::controlApi::ControlState;
use tempfile::TempDir;

/// 持有控制状态及其专属数据目录；字段顺序确保先释放状态，再清理证书、映射和录制文件。
pub struct ControlStateFixture {
    state: ControlState,
    _directory: TempDir,
}

impl Deref for ControlStateFixture {
    type Target = ControlState;

    /// 将测试夹具透明映射为控制状态，保持每个用例的调用点只关注控制面契约。
    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl ControlStateFixture {
    /// 返回当前测试夹具独占的数据根目录；调用方可在受管 mappings 等子目录写入真实夹具文件。
    ///
    /// 该路径只在夹具生命周期内有效，测试结束后由 `TempDir` 统一清理；调用方不得把它持久化到
    /// 夹具之外。函数不执行 I/O，因此不会产生额外失败分支。
    #[allow(dead_code)]
    pub fn dataDirectory(&self) -> &Path {
        self._directory.path()
    }
}

/// 创建具有独立证书、映射和录制目录的控制状态，避免并行用例写入真实用户数据或相互污染。
// 该支持文件会被多个独立集成测试二进制分别编译，因此单个测试二进制中的未使用告警不代表工作区死代码。
#[allow(dead_code)]
pub async fn newControlState() -> ControlStateFixture {
    let directory = tempfile::tempdir().expect("创建控制状态临时数据目录");
    let state = ControlState::newWithDataDirectory(directory.path())
        .await
        .expect("创建隔离控制状态");
    ControlStateFixture {
        state,
        _directory: directory,
    }
}
