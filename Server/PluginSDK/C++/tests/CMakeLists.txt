add_executable(trafficModSdkTests sdkTests.cpp)
target_link_libraries(trafficModSdkTests PRIVATE trafficMod::sdk)

if(MSVC)
    target_compile_options(trafficModSdkTests PRIVATE /W4 /permissive- /EHsc)
else()
    target_compile_options(trafficModSdkTests PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()

add_test(NAME trafficModSdkTests COMMAND trafficModSdkTests)
