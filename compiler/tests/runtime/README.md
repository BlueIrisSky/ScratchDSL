# 可选的运行时兼容性测试

`cargo test` 检查 Rust 编译器。`validate.cjs` 是额外的开发测试：使用独立安装的 Node 和官方 TurboWarp VM 源码，检查生成的 `.sb3` 能否被实际 VM 加载、序列化并执行。编译器和 Windows 发布包不需要 Node，也不包含这些测试依赖。

准备官方 [TurboWarp scratch-vm](https://github.com/TurboWarp/scratch-vm) 源码及其运行依赖后，先用 Rust CLI 构建 `examples/hello/sbdsl.buildsb` 和 `examples/extensions/sbdsl.buildsb`，在仓库根目录运行：

```text
node compiler/tests/runtime/validate.cjs target/validation/scratch-vm examples/hello/dist/hello.sb3 examples/extensions/dist/extensions.sb3
```

测试工具和依赖可放入已忽略的 `target/validation/`。首次验证使用的上游提交为 `e9c6e122bff25a13b57059e4fd32544e7d9560ca`；测试脚本本身不会下载或安装依赖。

断言包括资源文件存在且 MD5 正确、VM 序列化保留 opcode 图，以及基础示例的 `score = 10` / `x = 100`。扩展示例通过官方扩展加载与注册流程执行内嵌源码，检查 `x = 30`、报告值 `6` 和菜单实际值 `fast`。

这是无图形界面的执行测试：没有连接渲染器或音频引擎，不验证实际画面和声音。为支持官方的 unsandboxed 扩展加载器，脚本只模拟插入 `<script>` 的宿主操作，并且只接受项目内嵌的 JavaScript data URI；其余加载、扩展注册、积木执行和序列化均使用上游 VM。该宿主仅供仓库中受信任的测试示例使用。

过程测试保存在 [fixtures/procedures.sbdsl](../fixtures/procedures.sbdsl)。构建后，将其输出作为第四个位置参数传给验证脚本：

```text
sbdsl build compiler/tests/fixtures/procedures.sbdsl -o target/validation/outputs/procedures.sb3
node compiler/tests/runtime/validate.cjs target/validation/scratch-vm examples/hello/dist/hello.sb3 examples/extensions/dist/extensions.sb3 target/validation/outputs/procedures.sb3
```

该测试检查公共过程调用仍绑定全局数据和公共过程、私有同签名过程保留私有数据、递归执行三次，以及布尔参数只在为真时进入分支。预期全局 `score = 3`、私有 `score = 103`、`logs` 含三个 `tick`。

每个项目还会核对点击绿旗前的角色位置、方向、显示状态、旋转方式和当前造型，确认源码的静态初始化值在 VM 加载后保留。可以在过程测试输出之后再传入桌面演示 `.sb3`，检查其初始 `x = -100` 和“蓝色笑脸”造型，以及运行结束时的 `x = 100`、全局 `score = 20` 和成功提示。
