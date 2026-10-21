# 命令行

## 安装

从 [GitHub Releases](https://github.com/BlueIrisSky/ScratchDSL/releases/tag/v0.2.0) 下载并运行 Windows x64 安装包 `Install.exe`。它会把程序目录加入当前用户的 PATH，安装后重新打开终端即可使用 `sbdsl`，不需要管理员权限。

安装包可以单独离线分发，已经包含编译器、示例、文档和第三方许可。安装与卸载使用 Windows 自带的 PowerShell；使用编译器不需要 Rust、Cargo 或 Node.js。

重新安装时会先卸载旧版；旧版卸载失败就停止安装。卸载只移除程序自己的文件和它添加的 PATH 条目，保留用户原有的路径和自建文件。

免安装使用时，下载 `scratchdsl-0.2.0-windows-x64.zip` 并解压，通过程序路径运行，例如 `.\sbdsl.exe --version`。压缩包包含编译器、文档、示例、`LICENSE` 和 `THIRD_PARTY_NOTICES.txt`，转发编译器时请保留这两份许可文件。免安装包不修改 PATH。

发布页中的 `SHA256SUMS.txt` 列出了各个下载文件的 SHA-256。可用 `Get-FileHash .\Install.exe -Algorithm SHA256` 查看文件校验值并与之核对。

如果当前终端找不到已安装的命令，先关闭并重新打开终端或编辑器。

## 创建、编译和检查

```powershell
sbdsl --version
sbdsl init demo
sbdsl build demo/sbdsl.buildsb
sbdsl check demo/sbdsl.buildsb
```

`init` 生成 `sbdsl.buildsb`、`src/main.sbdsl` 和 `assets/sbdsl.svg`，并创建空的 `extensions/` 目录。初始源码只创建名为 `main` 的角色，位置为 `(-100, 0)`，造型使用 `.sbdsl` 的橙黄色积木图标，画布大小为 96 × 96。省略目录时使用当前目录；以上任一文件已存在时拒绝覆盖。默认模板输出为 `demo/dist/project.sb3`。

`build` 读取源码、扩展和资源，检查通过后写出 `.sb3`。`check` 执行相同检查，但不写文件。两者都会报告目标和积木数量。生成的文件可用 Scratch 兼容编辑器打开；包含第三方扩展时，编辑器还需要支持该扩展的加载方式。

```text
sbdsl init [目录]
sbdsl build [sbdsl.buildsb | 文件.sbdsl] [-o 输出.sb3]
sbdsl check [sbdsl.buildsb | 文件.sbdsl]
sbdsl lsp --stdio
sbdsl --version
```

`--output` 与 `-o` 相同，只能用于 `build`。`--version` 可缩写为 `-V`；不传命令或使用 `--help`、`-h`、`help` 会显示帮助。

`lsp --stdio` 供编辑器启动语言服务器，标准输入输出只传 LSP 消息，日志写到标准错误。通常由 [VS Code 插件](vscode.md)调用，不需要手动运行。

## 选择入口

| 命令中的输入 | 配置来源 | 编译入口 |
| --- | --- | --- |
| 不传输入 | 当前目录的 `sbdsl.buildsb` | 配置的 `entry` |
| 指定配置文件 | 指定文件 | 配置的 `entry` |
| 指定 `.sbdsl` | 从源码目录向上找最近的 `sbdsl.buildsb`，找不到就使用默认配置 | 指定的源码，覆盖配置中的 `entry` |

```powershell
sbdsl build demo/sbdsl.buildsb -o result.sb3
sbdsl build demo/src/main.sbdsl
```

第一条构建配置指定的工程；第二条把 `main.sbdsl` 作为入口。VS Code 的构建命令和实时诊断使用最近配置的 `entry`，打开哪个模块不会改变工程入口。

输出路径按 `-o`、配置的 `output`、入口同目录下同名 `.sb3` 的顺序选择。

## 路径

| 路径 | 相对位置 |
| --- | --- |
| 命令行输入和 `-o` | 运行命令时的当前目录 |
| 配置中的入口、输出、`srcDir`、扩展和舞台资源 | `sbdsl.buildsb` 所在目录 |
| 角色 `init` 中的造型和声音 | 声明这个 `init` 的源码文件所在目录 |

源码使用 UTF-8。扩展和资源只接受本地路径；字段和导入规则见[项目与配置](project.md)。

在源码仓库中运行示例可以用 `cargo run -- build examples/hello/sbdsl.buildsb`；开发环境和打包命令见[开发文档](development.md)。
