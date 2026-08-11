cmake_minimum_required(VERSION 3.21)
project(nativePassthrough LANGUAGES CXX)

add_library(nativePassthrough SHARED plugin.cpp)
target_compile_features(nativePassthrough PRIVATE cxx_std_20)
target_include_directories(nativePassthrough PRIVATE ../../include)
set_target_properties(nativePassthrough PROPERTIES PREFIX "")
