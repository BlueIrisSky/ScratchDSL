# ScratchDSL for VS Code

本地 Windows x64 插件，版本 0.1.0。支持 `.sbdsl` 的语法高亮、错误诊断、积木、变量、列表、自制积木和参数补全，以及构建和检查命令。

构建配置使用 `*.buildsb`，插件提供独立的 ScratchDSL Build 语言模式（`sbdsl-build`）。字段名不必加引号，每行一个字段即可；字符串值仍用双引号。配置和角色 `@init` 都支持字段、值与 `#` 注释高亮。根 `extensions` 数组支持 `"./自定义拓展.js" as "tools"`，其中 `as` 和命名空间分别着色。

插件默认通过绝对路径启动随包携带的 `server/sbdsl.exe lsp --stdio`，无需安装 Node.js，也不依赖 PATH 中的 `sbdsl`。可用 `scratchdsl.serverPath` 指定另一份编译器的绝对路径。暂不支持远程工作区、浏览器版 VS Code 或其他操作系统。

## 使用

`.sbdsl` 文件使用橙黄色积木图标，`.buildsb` 配置使用独立的橙黄色文件与小锤图标，两者均适配浅色和深色背景。若当前文件图标主题没有显示它们，可切换到 VS Code 自带的 Seti 主题。

输入 `$` 补全已声明变量，`$list:` 补全列表，`$global:` 和 `$global:list:` 选择全局数据。自制积木和内置积木一样从 `[` 开始补全，选中后按 Tab 填写参数；过程体内的 `@`、`<@` 可补全当前参数。没有弹出候选时按 `Ctrl+Space`。带空格等字符的名称会自动加引号。

候选读取未保存的源码、配置和导入模块，按当前角色筛选私有数据与过程。积木文字、声明名、参数、角色名、模块和扩展调用都有语法高亮，配色跟随当前主题；扩展前缀可以直接写 `自定义拓展.min.`，也可以使用配置中的别名。`@init` 提供属性与枚举补全，构建配置提供当前对象的属性与布尔值补全，包括嵌套的 `runtime`、`stage` 和资源对象，插入的字段名默认不带引号。

打开任意 `*.buildsb` 后，可点击编辑器右上角的构建按钮，精确使用这份配置。命令面板也提供：

- `ScratchDSL: 构建工程`
- `ScratchDSL: 检查工程`
- `ScratchDSL: 重启语言服务器`
- `ScratchDSL: 显示输出`

构建和检查会先保存相关工程内已修改的文件，包括配置。活动文件为 `*.buildsb` 时使用该文件，构建按钮使用其所在编辑器的配置；活动文件为源码时，才向上寻找最近的 `sbdsl.buildsb` 并使用其 `entry`。显式选择配置允许任意文件名，自动发现只识别默认名称 `sbdsl.buildsb`。没有配置时，使用活动 `.sbdsl` 作为入口。多根工作区没有活动源码或配置时会提示选择目录。构建结果和编译器错误显示在 ScratchDSL 输出面板中。

打开源码或构建配置后会启动语言服务器，未保存的源码和配置会同步给它。未命名的 ScratchDSL 文档也支持局部诊断与补全，构建前须保存为本地文件。修改源文件、扩展 `.js` 或配置文件会触发重新分析。

诊断通过 AST 检查入口和全部导入模块，一次显示多处语法与语义错误，未打开的导入文件也会检查。遇到错误后会保留可解析节点继续分析；错误修复、导入移除后，波浪线和“问题”面板会更新。严重破损的片段可能被跳过，部分错误仍标记整个积木。构建配置目前先报告一个错误。

工作区处于受限模式时，保留高亮和语言服务，但不运行项目 JavaScript 扩展，不允许构建或检查，忽略工作区设置中的自定义服务器路径。信任工作区后自动重启服务器；撤销信任由 VS Code 重新加载窗口处理。

`#` 兼作行注释与块注释，因此没有配置 VS Code 通用“切换行注释”快捷键。也不会自动补全 `/` 或 `<`，以免与菜单、除法和比较运算符冲突。`@init` 和构建配置支持完整 JSON 字符串转义，普通 DSL 字符串仍按自己的转义规则处理。

## 本地开发和验证

开发时需要 Node.js 22 或更新版本。所有命令在本目录执行：

```powershell
npm ci
npm test
```

`npm test` 执行 TypeScript 类型检查、esbuild 打包，以及使用 VS Code 官方 TextMate/Oniguruma 引擎的高亮回归测试。

将包含 `lsp --stdio` 的 Windows x64 Release 编译器放到 `server/sbdsl.exe` 后，运行：

```powershell
$env:VSCODE_EXECUTABLE_PATH = "$env:LOCALAPPDATA\Programs\Microsoft VS Code\Code.exe"
npm run test:host
```

该测试直接使用本机 VS Code，不下载编辑器。它在临时目录创建独立用户配置和中文工程，分别验证可信与受限工作区、未保存源码和配置的诊断、补全、构建/检查和重启。不会安装插件、修改现有 VS Code 用户设置或正式工程。测试结束删除临时目录。

对已打包文件，可运行 `node test/run-host.cjs --installed C:\完整路径\scratchdsl-0.1.0-win32-x64.vsix`。该模式仅向临时 extensions 目录安装 VSIX，再执行同一组验证，不影响用户已有插件。两种模式的编辑器日志保存在 `.vscode-test/logs/`。

`npm run compile` 输出 `dist/extension.js`。VSIX 打包由仓库发布流程统一处理；此目录不自动安装或发布插件。打包前必须加入编译器和 `server/THIRD_PARTY_NOTICES.txt`。

## 许可

ScratchDSL 插件使用 [MIT 许可证](https://github.com/BlueIrisSky/ScratchDSL/blob/main/LICENSE)，VSIX 内附有 `LICENSE.txt`。第三方依赖保留各自许可证，参见 `THIRD_PARTY_NOTICES.txt`，Rust 编译器依赖声明随服务器文件提供。

官方实现参考：[语言服务器客户端](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)、[工作区信任](https://code.visualstudio.com/api/extension-guides/workspace-trust)、[TextMate 高亮](https://code.visualstudio.com/api/language-extensions/syntax-highlight-guide)、[Extension Host 测试](https://code.visualstudio.com/api/working-with-extensions/testing-extension)。
