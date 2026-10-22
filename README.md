# ScratchDSL

用文本编写 Scratch 项目，编译成 `.sb3`。编译器使用 Rust，当前版本为 `0.2.0`。

**0.2.0 是过渡预发布版。** 本版包含较大的、不兼容旧代码的语法变化，后续过渡版本的语法仍可能大幅调整。升级前请备份源码，按[语法文档](docs/syntax.md)修改项目；变更概要见[发布说明](docs/releases/0.2.0.md)。

## 开始使用

从 [Releases](https://github.com/BlueIrisSky/ScratchDSL/releases/tag/v0.2.0) 下载 Windows x64 安装包 `Install.exe`。安装后重新打开终端，执行：

```powershell
sbdsl init demo
sbdsl build demo/sbdsl.buildsb
```

编译结果在 `demo/dist/project.sb3`。`sbdsl check demo/sbdsl.buildsb` 可以检查项目而不生成文件。

角色脚本这样写：

```scratch
sprite "玩家":
    init:
        x: -100
    end
    start:
        移动(10)步
    end
end
```

变量赋值直接写 `score = 10`、`score += 1`，右侧可写 `score = a+b*2`，按运算优先级生成积木。完整写法见[语法文档](docs/syntax.md)。

数据名只允许普通标识符，直接赋值推断类型：`score = 0` 是变量，`logs = {}` 是列表，不使用 `var`、`list` 声明。普通输入都必须加圆括号，例如 `移动(score)步`、`将(score)加入(logs)`；布尔输入使用 `<...>`。首次赋值确定种类，变量与列表不能互转，错误会在 IDE 标红。

第三方扩展的 C 型积木支持 `扩展.文字(参数):` 和缩进体，多个分支用同级 `else:` 接续，最后用同级 `end` 结束。编译器按扩展元信息映射输入参数和各分支，详见[扩展文档](docs/extensions.md#c-型控制积木)。不挂事件帽子的独立积木段写成 `piece:` 后换行缩进，并以同级 `end` 结束；它不会自动运行，只会作为素材编译进 `.sb3`。

同一发布页提供免安装包 `scratchdsl-0.2.0-windows-x64.zip` 和文件校验值 `SHA256SUMS.txt`。安装包可独立离线安装；CLI 和语言服务器均为独立 Rust 程序，使用时无需安装 Rust 或 Node.js。

VS Code 插件可通过“从 VSIX 安装…”加载发布页中的 `scratchdsl-0.2.0-win32-x64.vsix`。打开 `.sbdsl` 后可使用高亮、错误提示和积木补全，按 `Ctrl+Space` 手动唤起补全。打开 `sbdsl.buildsb` 后，可点击编辑器右上角的构建按钮。插件自带编译器。

## 文档

| 文档 | 内容 |
| --- | --- |
| [命令行](docs/cli.md) | 安装、创建项目、编译和检查 |
| [语法](docs/syntax.md) | 注释、字面量、积木、菜单、运算符和控制流 |
| [角色与自制积木](docs/sprites.md) | `sprite`、`init`、作用域、参数和过程调用 |
| [项目与配置](docs/project.md) | `import`、文件合并和 `sbdsl.buildsb` |
| [积木表](docs/blocks.md) | 内置积木写法与 opcode |
| [本地扩展](docs/extensions.md) | 加载扩展、命名空间和显式 opcode 调用 |
| [VS Code](docs/vscode.md) | 插件安装、命令、设置和已知限制 |
| [开发](docs/development.md) | 源码位置、构建、测试和打包 |
| [模板匹配](docs/templates.md) | 模板数据、扩展元信息和匹配规则 |
| [SB3 生成](docs/sb3.md) | 积木图、资源和 `project.json` 编码 |
| [语言服务器](docs/lsp.md) | 文本同步、诊断、补全和扩展缓存 |

## 从源码运行

`compiler/` 放 Rust 编译器和语言服务器，`vscode/` 放插件，`packaging/` 放安装器与打包脚本。测试跟随各自组件；文档和示例分别在 `docs/`、`examples/`。

在仓库根目录运行：

```sh
cargo run -- build examples/hello/sbdsl.buildsb
cargo test --locked
```

[基础示例](examples/hello/main.sbdsl)和[扩展示例](examples/extensions/main.sbdsl)在 `examples/`。发行文件放在 `dist/`，构建缓存和本地测试资料放在 `target/`，这两个目录都已排除 Git。

GitHub Actions 在 Linux 和 Windows 上检查 Rust 代码，并在 Windows 上测试和打包插件。发布标签 `v0.2.0` 时，检查通过后会自动上传安装包、免安装包和 VSIX，标记为过渡预发布版，流程见[开发文档](docs/development.md#自动检查与发布)。

## 许可

ScratchDSL 使用 [MIT 许可证](LICENSE)。第三方依赖保留各自的许可证，发行包附有 `THIRD_PARTY_NOTICES.txt`。
