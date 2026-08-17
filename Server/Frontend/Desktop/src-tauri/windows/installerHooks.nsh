!macro NSIS_HOOK_POSTINSTALL
  ; 旧安装曾把完整 Android 源码复制到安装目录；新架构只保留预编译模板和独立打包器。
  ; 安装完成后删除确定的旧资源目录，避免目标电脑继续占用源码或被误认为需要本地编译环境。
  RMDir /r "$INSTDIR\clientProject"
!macroend
