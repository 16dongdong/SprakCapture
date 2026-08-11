add_library(binaryTransformer SHARED plugin.cpp)
target_link_libraries(binaryTransformer PRIVATE trafficMod::sdk)
set_target_properties(binaryTransformer PROPERTIES PREFIX "")

# Host 以清单目录作为相对入口根；构建后把清单与动态库放在同一目录，产物可直接加载。
add_custom_command(TARGET binaryTransformer POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        ${CMAKE_CURRENT_SOURCE_DIR}/plugin.json
        $<TARGET_FILE_DIR:binaryTransformer>/plugin.json
    VERBATIM
)
