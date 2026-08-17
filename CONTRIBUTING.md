# 参与贡献

感谢参与 Sprak Capture。

1. Fork 仓库并从 `main` 创建职责单一的分支。
2. 修改前阅读相关模块 README、公开协议和调用方。
3. 为行为变更补充测试，并执行格式化、静态检查和受影响测试。
4. 提交 Pull Request，说明根因、实现、验证命令和兼容性影响。

不得提交凭据、证书私钥、抓包正文、运行数据库、日志或构建产物。用户可见文本应走现有国际化机制；冻结 ABI 与协议字段不得静默变更。

```powershell
# 服务端项目
cd Server
pnpm check
pnpm test

# Android 客户端项目
cd ..\Client
.\gradlew.bat :app:testDebugUnitTest :app:lintDebug :app:assembleDebug `
  "-PclientNodeHost=127.0.0.1" `
  "-PclientNodePort=1080"
```

两个项目只通过 [`Contracts/clientServerContract.md`](Contracts/clientServerContract.md) 约定的线协议协作；
提交跨项目变更时必须分别完成两套验证，禁止通过源码引用把两个构建系统重新耦合。
