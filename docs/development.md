# 开发与打包

仓库根目录是 Cargo workspace，默认成员为 `compiler/`。CLI 和 LSP 在同一个 Rust package 中，共用解析器、模板表及语义绑定代码；`vscode/` 是 TypeScript 客户端，通过 stdio 连接编译器。

## 源码位置

| 文件或目录 | 内容 |
| --- | --- |
| [`compiler/src/main.rs`](../compiler/src/main.rs) | CLI 参数、工程初始化、LSP 启动 |
| [`compiler/templates/main.sbdsl`](../compiler/templates/main.sbdsl) | `sbdsl init` 生成的初始源码 |
| [`compiler/src/model.rs`](../compiler/src/model.rs)、[`compiler/src/parser.rs`](../compiler/src/parser.rs) | AST、源码范围和解析 |
| [`compiler/src/config.rs`](../compiler/src/config.rs)、[`compiler/src/project.rs`](../compiler/src/project.rs) | 配置发现、模块展开、声明合并、角色创建与追加 |
| [`compiler/src/registry.rs`](../compiler/src/registry.rs)、[`compiler/src/builtins.rs`](../compiler/src/builtins.rs)、[`compiler/src/extensions.rs`](../compiler/src/extensions.rs) | 模板匹配、内置积木、扩展元信息 |
| [`compiler/src/analysis/`](../compiler/src/analysis/) | 文件来源、未保存文本、Binder、扩展元信息缓存 |
| [`compiler/src/codegen.rs`](../compiler/src/codegen.rs)、[`compiler/src/build.rs`](../compiler/src/build.rs) | 积木图、资源处理、SB3 打包 |
| [`compiler/src/lsp/`](../compiler/src/lsp/) | 协议、文档同步、诊断、补全、UTF-16 位置转换 |
| [`vscode/`](../vscode/) | 插件客户端、TextMate 高亮、编辑配置和测试 |
| [`compiler/tests/`](../compiler/tests/) | 编译集成测试、LSP 协议测试、VM 测试 |
| [`packaging/`](../packaging/) | CLI 安装包和 VSIX 打包脚本 |
| [`packaging/windows/`](../packaging/windows/) | Windows 安装器、PATH 辅助脚本及其测试 |

解析和模块合并之后，Binder 处理声明覆盖、变量与列表引用、自制积木的词法作用域、槽位类型和目标限制。`build` 与 `check` 随后生成积木图并校验资源，只有 `build` 写出 `.sb3`。LSP 在未保存文本上运行同一套分析，但不生成积木图或解码资源，补全也直接读取 Rust 模板表。具体实现见 [LSP](./lsp.md) 和 [SB3](./sb3.md)。

构建配置和 `init` 共用数据语法：裸字段名、冒号、换行或逗号分隔，字符串与数字沿用 JSON 字面量。字段类型由 `serde` 校验，SB3 的 JSON 使用 `serde_json`，ZIP 使用 `zip`。本地扩展通过 `rquickjs` / QuickJS-NG 执行 `getInfo()`；编译器提供受支持的 `Scratch` 桩接口，JavaScript 只用于扩展元信息提取，见[模板实现](./templates.md)。编译器只做源码到 SB3 的正向编译，没有反编译功能。

## 开发环境

CI 使用 Rust 1.98.1，在 Linux 和 Windows 上构建、测试编译器。QuickJS 包含 C 代码，Linux 构建机需要 C 编译器，Windows 构建机需要 MSVC 本机编译环境。[`.cargo/config.toml`](../.cargo/config.toml) 对 `x86_64-pc-windows-msvc` 设置了 `-C target-feature=+crt-static`，静态链接 CRT。

插件开发需要 Node.js 22 或更新版本和 npm，CLI 安装器使用 NSIS，VSIX 使用插件开发依赖中的 `@vscode/vsce`。Rust 与 npm 依赖分别由 [`Cargo.lock`](../Cargo.lock) 和 [`package-lock.json`](../vscode/package-lock.json) 固定。用户运行编译器或安装 VSIX 时无需安装这些开发工具。

## 测试

在仓库根目录运行 Rust 检查和示例，Cargo 会使用 `compiler/Cargo.toml`，构建结果仍写入根目录的 `target/`：

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -- build examples/hello/sbdsl.buildsb
cargo run --locked -- check examples/extensions/sbdsl.buildsb
```

测试包括解析、绑定、共享分析、生成结构、[`syntax.rs`](../compiler/tests/syntax.rs) 和 [`end_to_end.rs`](../compiler/tests/end_to_end.rs)。[`compiler/tests/lsp.rs`](../compiler/tests/lsp.rs) 启动真实编译器进程验证协议。示例输出保存在各自的 `dist/` 中。

安装器的 PATH 测试使用隔离的测试注册表，不改真实用户 PATH：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/windows/tests/path.ps1
```

发行前还需验证安装、`init`、`build`、`check`、本地扩展加载和卸载。现有 Windows 主机已经验证静默安装、安装后构建、扩展检查和卸载；尚未在独立的干净系统验收。

需要检查实际 VM 执行结果时，准备官方 TurboWarp VM 源码及其 Node.js 依赖，构建两个示例后运行：

```powershell
cargo run --locked -- build examples/extensions/sbdsl.buildsb
node compiler/tests/runtime/validate.cjs target/validation/scratch-vm examples/hello/dist/hello.sb3 examples/extensions/dist/extensions.sb3
```

VM 测试检查加载、序列化、资源 MD5、脚本执行和角色静态初值，不连接渲染器或音频引擎。测试依赖不打入发行包。过程测试和可选桌面演示参数见[运行时测试说明](../compiler/tests/runtime/README.md)。

在 `vscode/` 中检查插件：

```powershell
npm ci
npm test
```

`npm test` 先做 TypeScript 类型检查和 esbuild 打包，再用 VS Code 的 TextMate / Oniguruma 引擎测试高亮。也可单独运行 `npm run compile`，输出为 `dist/extension.js`。

将 Windows x64 release 编译器复制到插件的 `server/sbdsl.exe` 后，在插件目录运行 extension-host 测试：

```powershell
env:VSCODE_EXECUTABLE_PATH = "env:LOCALAPPDATA\Programs\Microsoft VS Code\Code.exe"
npm run test:host
```

测试使用本机 VS Code，创建临时用户配置和中文工程，覆盖可信与受限工作区、未保存源码与配置、补全、构建、检查和重启。它不下载编辑器或修改现有用户设置，结束后删除临时目录；日志保存在插件的 `.vscode-test/logs/`。

检查打包后的 VSIX 时，在插件目录运行：

```powershell
node test/run-host.cjs --installed C:\完整路径\scratchdsl-0.4.0-win32-x64.vsix
```

这个模式将 VSIX 安装到临时 extensions 目录，再运行同一组测试，不影响已有插件。

## CLI 安装包

在仓库根目录的 Windows PowerShell 运行：

```powershell
.\packaging\package.ps1
```

找不到 NSIS 时可以指定位置：

```powershell
.\packaging\package.ps1 -Makensis "C:\Program Files (x86)\NSIS\makensis.exe"
```

[`package.ps1`](../packaging/package.ps1) 执行 `cargo test --locked` 和 `cargo build --release --locked`，复制编译器、收集第三方许可，再调用 [`ScratchDSL.nsi`](../packaging/windows/ScratchDSL.nsi)。`-CompilerOnly` 只生成编译器和许可说明；`-SkipBuild` 复用已有 release 文件，使用前需确认它对应当前源码且已经通过检查。

输出为 `dist/sbdsl.exe`、`dist/LICENSE`、`dist/THIRD_PARTY_NOTICES.txt` 和 `dist/Install.exe`。安装版只需分发一个 `Install.exe`：编译器、PATH 辅助脚本、文档、示例和许可文件都已内嵌，安装时不下载依赖。安装与卸载调用 Windows 自带的 PowerShell。单独分发 `sbdsl.exe` 时，应附上 `LICENSE` 和 `THIRD_PARTY_NOTICES.txt`。

安装器采用当前用户安装，不需要管理员权限，提供安装目录选择、命令行快捷方式和卸载项。它把安装目录加入用户 `PATH`，保留原有路径、变量引用和注册表值类型，不重复追加同一目录，并通知系统环境变化。已有终端或编辑器可能需要重启。

重新安装时，用户确认开始安装后，安装器先同步卸载已注册旧版；卸载失败则停止。卸载只移除清单内的程序文件，以及安装器实际添加并记录归属的 PATH 条目。用户原有的同路径条目和自建项目会保留。开发测试、缓存和示例编译结果不在安装清单中。

## VSIX

在仓库根目录运行：

```powershell
.\packaging\package-vscode.ps1
```

[`package-vscode.ps1`](../packaging/package-vscode.ps1) 调用编译器打包步骤，将 EXE 和更新后的 Rust 依赖许可复制到插件 `server/`，执行 `npm ci` 和插件测试，核对编译器与插件版本一致，再运行 `vsce package --target win32-x64`，生成稳定版 VSIX。输出为 `dist/scratchdsl-0.4.0-win32-x64.vsix`。`-Npm` 可指定 npm，`-SkipBuild` 复用已经验证的 release 编译器。

包内包含语言文件、客户端 bundle、原生 EXE、MIT 许可证和第三方许可，不包含源码、`node_modules`、测试工程或构建缓存。打包脚本不会自动安装插件或发布到 Marketplace。成品只支持本地 Windows x64，其他系统、远程工作区和浏览器版尚未支持。

## 自动检查与发布

[CI and Release](../.github/workflows/ci.yml) 在推送 `main`、创建或更新 PR，以及推送 `v*` 标签时运行，也可以在 Actions 页面手动启动。

- Linux 和 Windows：运行 Rust 格式检查、Clippy、测试和示例编译。
- Windows：运行插件类型检查、高亮测试、打包后的 VSIX extension-host 测试，以及隔离注册表中的 PATH 测试；构建安装包与免安装包。
- 推送版本标签：检查标签、编译器、插件和安装器版本一致，全部检查通过后创建 GitHub Release 并上传文件。发布说明读取 `docs/releases/<版本>.md`。GitHub Release 作为稳定版发布，并设为 Latest。手动启动工作流只检查和打包。

当前稳定版本为 `0.4.0`，对应标签 `v0.4.0`。发布页提供：

| 文件 | 用途 |
| --- | --- |
| `Install.exe` | Windows x64 独立离线安装包，自动配置当前用户 PATH |
| `scratchdsl-0.4.0-windows-x64.zip` | Windows x64 免安装包，包含 CLI、文档、示例和第三方许可 |
| `scratchdsl-0.4.0-win32-x64.vsix` | Windows x64 VS Code 插件，自带 CLI 和语言服务器 |
| `sbdsl.exe` | 单独的 Windows x64 编译器，分发时应附下面两份许可文件 |
| `SHA256SUMS.txt` | 下载文件的 SHA-256 校验值 |
| `release-manifest.json` | 版本、构建来源和文件信息 |
| `LICENSE` | ScratchDSL 的 MIT 许可证 |
| `THIRD_PARTY_NOTICES.txt` | CLI 使用的第三方依赖许可说明 |

本地准备同样的发布文件时，先运行两个打包脚本，再运行：

```powershell
.\packaging\prepare-release.ps1
```

脚本使用 7-Zip 解开安装包，核对安装包、免安装包和 VSIX 中的编译器及 MIT 许可证是否一致，再生成发布清单和校验值。7-Zip 需在 PATH 或默认安装目录中，也可通过 `-SevenZip` 指定程序路径。发布文件通过 GitHub Releases 分发，不提交到源码仓库。

## 生成文件与许可

仓库根目录的 `dist/` 保存安装包、编译器、VSIX、免安装包、校验值和许可说明。Cargo 产物、开发工具、缓存与临时验证文件放在 `target/`，验证报告放在 `target/validation/reports/`。

根目录的 [`.gitignore`](../.gitignore) 排除 `target/`、各级 `dist/`、`.sb3` / `.exe` / `.pdb` / `.vsix`、临时文件、日志和 `node_modules/`；[插件的忽略规则](../vscode/.gitignore) 另排除服务器文件、生成的许可说明和 `.vscode-test/`。`Cargo.lock`、`.cargo/config.toml` 和插件锁文件保留在 Git 中。

ScratchDSL 使用根目录的 [MIT 许可证](../LICENSE)，编译器与插件的包元数据均标记为 `MIT`。打包脚本将许可证复制到发行文件和插件目录，生成的副本不提交到 Git。第三方依赖沿用各自的许可证：CLI 打包时收集 Rust 依赖说明，插件构建时生成客户端依赖说明，VSIX 同时携带两份 `THIRD_PARTY_NOTICES.txt`。
