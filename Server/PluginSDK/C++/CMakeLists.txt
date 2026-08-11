cmake_minimum_required(VERSION 3.20)

project(trafficModSdk LANGUAGES CXX)

add_library(trafficModSdk STATIC
    src/json.cpp
    src/runtime.cpp
)
add_library(trafficMod::sdk ALIAS trafficModSdk)

target_compile_features(trafficModSdk PUBLIC cxx_std_20)
target_include_directories(trafficModSdk PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:include>
)
set_target_properties(trafficModSdk PROPERTIES POSITION_INDEPENDENT_CODE ON)

if(MSVC)
    target_compile_options(trafficModSdk PRIVATE /W4 /permissive- /EHsc)
else()
    target_compile_options(trafficModSdk PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()

option(TRAFFIC_MOD_BUILD_EXAMPLES "构建 SDK 示例插件" ON)
option(TRAFFIC_MOD_BUILD_TESTS "构建 SDK 独立测试" ON)
option(TRAFFIC_MOD_STATIC_MINGW_RUNTIME "把 MinGW C++ 运行库静态链接进插件" ON)

# Native 插件通常只分发单个动态库；MinGW 默认的三个运行库依赖会导致宿主机器加载失败。
# 该选项只影响最终插件链接，MSVC 继续遵循工程自身选择的 /MD 或 /MT 运行库策略。
if(MINGW AND TRAFFIC_MOD_STATIC_MINGW_RUNTIME)
    target_link_options(trafficModSdk INTERFACE -static -static-libgcc -static-libstdc++)
endif()

if(TRAFFIC_MOD_BUILD_EXAMPLES)
    add_subdirectory(examples/binaryTransformer)
endif()

if(TRAFFIC_MOD_BUILD_TESTS)
    enable_testing()
    add_subdirectory(tests)
endif()
