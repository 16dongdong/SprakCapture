// Command sprak-plugin-gen 为 Go c-shared 插件生成固定 Native ABI v2 桥接文件。
package main

import (
	"flag"
	"fmt"
	"os"

	"sprakcapture/plugin-sdk-go/bridgegen"
)

// main 解析生成参数并写入桥接文件；失败时返回非零进程状态供 go generate/CI 检测。
func main() {
	output := flag.String("output", ".", "生成目录")
	packageName := flag.String("package", "main", "目标 Go 包名")
	importPath := flag.String("sdk-import", "sprakcapture/plugin-sdk-go", "SDK 导入路径")
	flag.Parse()
	if err := bridgegen.Generate(bridgegen.Options{
		OutputDirectory: *output,
		PackageName:     *packageName,
		SDKImportPath:   *importPath,
	}); err != nil {
		_, _ = fmt.Fprintf(os.Stderr, "生成 Native ABI v2 桥失败：%v\n", err)
		os.Exit(1)
	}
}
