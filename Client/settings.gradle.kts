pluginManagement {
    // Kotlin 2.4 元数据要求 R8 9.1.29 以上；AGP 8.13 内置版本会在发布压缩时跳过元数据解析。
    // 仅覆盖 D8/R8 工具本身，版本取自 Android 官方兼容矩阵，不改变应用运行时依赖。
    buildscript {
        repositories {
            mavenCentral()
            maven("https://storage.googleapis.com/r8-releases/raw")
        }
        dependencies {
            classpath("com.android.tools:r8:9.1.34")
        }
    }
    repositories {
        mavenCentral()
        google()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        // 国内网络访问 Google Maven 会长时间超时；镜像仅承载 AndroidX/Android 构件，
        // 其余依赖仍由 Maven Central 提供，避免扩大第三方仓库的解析范围。
        maven("https://maven.aliyun.com/repository/google") {
            name = "googleMirror"
            content {
                includeGroupByRegex("androidx\\..*")
                includeGroupByRegex("com\\.android\\..*")
            }
        }
        mavenCentral()
        google()
    }
}

rootProject.name = "ProxyClient"
include(":app")
