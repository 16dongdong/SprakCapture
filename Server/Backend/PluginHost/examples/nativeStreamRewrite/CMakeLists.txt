cmake_minimum_required(VERSION 3.21)
project(nativeStreamRewrite LANGUAGES CXX)

add_library(nativeStreamRewrite SHARED plugin.cpp)
target_compile_features(nativeStreamRewrite PRIVATE cxx_std_20)
target_include_directories(nativeStreamRewrite PRIVATE ../../include)
set_target_properties(nativeStreamRewrite PROPERTIES PREFIX "")
