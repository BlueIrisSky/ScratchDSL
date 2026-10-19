# 模板引擎

内置积木和第三方扩展共用注册表与匹配器，代码在 [`compiler/src/registry.rs`](../compiler/src/registry.rs) 和 [`compiler/src/extensions.rs`](../compiler/src/extensions.rs)。普通调用按模板文字匹配，指定 opcode 的调用直接读取该模板的槽位定义。扩展的参数定义只来自 `getInfo()`，不另维护一套 DSL 参数表。

## 数据结构

注册表定义在 [`compiler/src/registry.rs`](../compiler/src/registry.rs)，模板类型定义在 [`compiler/src/model.rs`](../compiler/src/model.rs)。

| 类型 | 字段 | 用途 |
| --- | --- | --- |
| `Registry` | `templates: Vec<Template>` | 内置与扩展模板；可见的自制积木模板另传给匹配器 |
| `Registry` | `extension_sources: BTreeMap<String, String>` | 运行时扩展 ID 到本地 JavaScript 源码的映射，供打包内嵌 |
| `Template` | `opcode: String` | 写入 `.sb3` 的运行时 opcode |
| `Template` | `shape: Shape` | 调用形状：`Command`、`Reporter`、`Boolean`、`Hat` 或 `Control`；扩展 `hat`、`event` 都映射为 `Hat` |
| `Template` | `namespace`、`extension_id`、`local_opcode: Option<String>` | DSL 命名空间（文件名或 `as` 别名）、运行时扩展 ID、扩展内 opcode；内置积木不设置这些字段 |
| `Template` | `tokens: Vec<Token>` | 固定文字与槽位，按显示文字中的顺序排列 |
| `Template` | `sprite_only`、`stage_only: bool` | 是否限制在角色或舞台上使用 |
| `Template` | `procedure: Option<String>`、`warp: bool` | 自制积木的稳定源码标识及运行模式；内置与扩展分别为 `None`、`false` |
| `Slot` | `name: String` | 参数名，也用于生成积木的输入或字段名；自制积木调用另映射为参数 ID |
| `Slot` | `arg_type: ArgType` | `Any`、`String`、`Number`、`Boolean`、`Color`、`Angle`、`Variable`、`List` 或 `Broadcast` |
| `Slot` | `storage: Storage` | `Input` 写入 `inputs`，`Field` 写入 `fields` |
| `Slot` | `default: serde_json::Value` | 默认值；`null` 表示未提供可用默认值 |
| `Slot` | `shadow: Option<Shadow>`、`menu: Option<Menu>` | shadow 编码说明与菜单校验信息 |
| `Shadow` | `opcode: String`、`field: String` | 独立 shadow 积木的 opcode 与值所在字段 |
| `Shadow` | `primitive: Option<u8>` | 使用紧凑原始值编码时的类型编号，普通 shadow 使用 4 至 10；`None` 表示生成独立积木 |
| `Menu` | `items: Option<Vec<(String, String)>>` | 静态菜单的 `(显示文字, 实际值)` 列表；`None` 表示动态菜单 |
| `Menu` | `accept_reporters: bool` | 是否允许表达式输入 |

`Token` 只有 `Literal(String)` 和 `Slot(Slot)` 两种。槽位在 `tokens` 中的出现顺序决定显式 opcode 调用的参数顺序，不取 `arguments` 的键顺序。

扩展元信息中的 `blockType`、`defaultValue`、`acceptReporters` 等字段由加载器转换为上述类型，不直接作为 Rust 字段写入目标项目。菜单的 `acceptReporters: false` 对应 `Storage::Field`，`true` 对应 `Storage::Input`。菜单信息放在 `Slot.menu`，shadow 放在 `Slot.shadow`。

变量、列表和广播分别用 `ArgType::Variable`、`ArgType::List`、`ArgType::Broadcast` 区分。广播编码保留名称与 ID 的关系，广播输入的 shadow 使用编号 11，不走普通字符串或 `Shadow.primitive` 的编码。输出格式见 [SB3](./sb3.md)。

## 支持的类型

扩展支持 `command`、`reporter`、`Boolean`、`hat`、`event`，参数支持 `string`、`number`、`Boolean`、`color`、`angle`。`ArgType::Variable`、`ArgType::List`、`ArgType::Broadcast` 用于内部模板，不作为扩展参数类型开放。

`conditional`、`loop`、分支子栈、多段 `text` 数组及 `matrix`、`note`、`image`、`costume`、`sound` 等类型在注册时报告错误，附来源、opcode 和参数名，不自动改成其他类型。原版 C 型控制流由专用语法处理。工具箱分隔符、`label`、`button` 不生成模板。

输入类型决定默认 shadow 和编辑器形状，不要求编译器执行运行时类型转换：

| 槽位 | 可接受的源码值 |
| --- | --- |
| 普通非布尔输入 | 数字、字符串、变量、报告积木、有值的布尔积木 |
| 颜色输入 | 直接字面量须为 `"#rrggbb"`，六位十六进制不区分大小写；报告输入的运行时颜色由扩展处理 |
| 角度输入 | 与普通数值槽相同 |
| 布尔输入 | 布尔积木、布尔参数、`<true>`、`<false>` 或未连接输入 `<>` |
| 直接变量/列表字段 | 对应的符号引用 |

`<>` 只在布尔槽内表示空输入，不能单独求值；`ext::"ready".<>` 则是零参数布尔调用。布尔槽没有字面量 shadow。所有源码参数都要提供，元信息中的默认值不允许省参。

## 读取扩展

加载器从 `sbdsl.buildsb.extensions` 读取本地 `.js`，按文件身份去重，并按文件名确定命名空间，规则见[扩展](./extensions.md)。它不接受 URL，不执行网络下载。已读取的源码留待打包时内嵌到 `extensionURLs`。

Rust 通过 rquickjs 0.14 嵌入 QuickJS-NG，不依赖 Node.js。每个来源创建独立的 `Runtime` 和 `Context`，执行一次源码，提取 `getInfo()` 后释放引擎。

提供给扩展的宿主接口如下：

- `Scratch.ArgumentType`、`Scratch.BlockType`、`Scratch.TargetType` 常量，包含暂未支持但需要诊断的类型名称。
- `Scratch.extensions.register` 捕获回调。`Scratch.extensions.unsandboxed = true` 供初始化兼容分支判断，不表示存在完整 VM。
- `Scratch.translate`：字符串原样返回，对象取 `default`，缺省为空字符串；`translate.setup` 为空操作。
- `Scratch.vm.runtime.on`：仅供初始化监听使用的空桩。
- `window`、`self`：引用当前 `globalThis`，没有 DOM 或浏览器 API。
- JavaScript 引擎自带的 `Math`、`JSON`、集合、类型化数组等；`console` 日志由宿主接收。定时器和未实现的宿主 API 在调用时报告不支持。

源码必须同步注册且恰好注册一个实例。捕获 `register(instance)` 时同步调用一次 `instance.getInfo()`，结果必须是对象。零次、多次、异步注册或返回 Promise 均报错，不通过执行待处理任务等待异步结果。

在上下文释放前逐项验证元信息，转换为 Rust 拥有的数据。动态菜单验证方法名后以 `Menu.items = None` 标记，不执行或保存该方法；不能靠序列化整个实例、静默丢掉函数或 `undefined` 来代替校验。可选字段缺省时按规则补齐，非法必需字段报错。最终保留来源、命名空间、源码字节及验证后的元信息，注册表和 AST 中的数据不依赖 JavaScript 上下文生命周期。

缺少接口、脚本异常及非法元信息均生成诊断，附扩展路径、执行阶段和可获得的源码位置。

## 编译模板

遍历 `getInfo().blocks`，跳过分隔符、标签和按钮。每个脚本积木先检查类型，再按以下规则生成模板。

### 身份与文字

`info.id` 和 `block.opcode` 必须是满足运行时注册约束的非空字符串。模板记录配置确定的命名空间（文件名或 `as` 别名）、`extension_id = info.id`、`local_opcode = block.opcode`，以及 `opcode = "${info.id}_${block.opcode}"`。命名空间不替换真实 opcode 中的扩展 ID。

`block.text` 必须是单个字符串。用 `/\[([A-Za-z_][A-Za-z0-9_]*)\]/g` 识别占位符，切成固定文字和槽位：

- `[ARG_NAME]` 查找 `block.arguments[ARG_NAME]`。每个参数在文字中必须出现一次，未声明、重复、遗漏或名称非法都报错。
- 有 `arg.menu` 时建立 `Menu`，由菜单设置确定 `storage`；其他参数使用 `Storage::Input`，记录类型、默认值和 shadow。
- 按文字中的位置排列槽位。`[ARG_NAME]` 是 JavaScript 元数据占位符；DSL 菜单值统一写成 `/'选项'/`，内容必须使用单引号。
- 固定文字的连续语法空白归成一个空格，只忽略整个模板首尾空白，保留参数分隔。匹配时该空格要求源码至少有一个语法空白；内置中缀运算符 `+ - * / mod < > =` 两侧的空白除外，但仍须保留数值和名称的词法边界。字符串、菜单内容和名字不做空白归一化。

将 `block.blockType` 转为 `shape`，目标限制转为 `sprite_only`、`stage_only`；`hat`、`event` 都用帽子语法。内置模板也记录角色/舞台限制；公共过程实例化时逐个检查体内调用是否适用于该目标。

### 菜单

`info.menus[name]` 可以是数组或对象。数组等价于 `acceptReporters: false` 的静态菜单；对象省略 `acceptReporters` 时也取 `false`。对象的 `items` 是数组时为静态菜单，是方法名字符串时为动态菜单，其他形式报错。

静态菜单至少包含一项。字符串 `"left"` 转成 `{text: "left", value: "left"}`，对象项保留两个字符串。重复的 `value` 可以合并，但要保留各个显示别名。

菜单字面量解码后先精确查 `value`，未命中才查 `text`。显示文字只对应一个值时转换为该值，对应多个不同值时报歧义，没有匹配时报非法选项。实际值的精确匹配优先于其他项的同文显示文字。原版 `/'鼠标指针'/`、`/'全部脚本'/` 等中文选项也通过别名映射到实际存储值。

动态菜单的方法名通过检查后，在模板中记为 `Menu.items = None`，不调用它来枚举项目。源码的 `/'选项'/` 直接作为实际值，不做成员检查或显示文字转换。

| 菜单设置 | 可接受的值 | 编码 |
| --- | --- | --- |
| `acceptReporters: false` | 仅 `/'...'/` 字面量 | 父块字段 |
| `acceptReporters: true` | 菜单字面量，以及字符串、数字、变量、报告积木、有值的布尔积木；不含 `<>` | 输入 |

普通表达式的运行时值可以不在菜单中，因此不做静态成员检查。变量、列表、广播符号字段仍使用各自语法，不能用普通菜单替代。

`defaultValue` 使用实际值。静态菜单未指定默认值时取首项；动态菜单未指定时不执行方法推断。显式菜单值用自身值建立 shadow；报告积木占据输入时使用菜单默认值，无默认值则不生成 shadow。默认值不允许源码省略参数。

菜单元信息对应 [TurboWarp 输入文档](https://docs.turbowarp.org/development/extensions/inputs)，shadow 与运行时编码参照 [VM 的输入转换实现](https://github.com/TurboWarp/scratch-vm/blob/develop/src/engine/runtime.js)。

### 注册

同一来源只注册一次。不同来源的命名空间、`info.id` 任一重复都报错；同一扩展内的 `local_opcode` 也不能重复。

完整 opcode 要与原版、其他扩展及该扩展的其他条目检查冲突，还须包含自动生成的 `${info.id}_menu_${menuName}` 菜单块。多个参数引用同一菜单时只注册一次。命名空间不改变运行时身份，不能用于放行这些冲突。

通过检查的模板加入注册表。匹配时按命名空间和调用形状筛选候选：`[...]`、`(...)`、`<...>` 或帽子。原版模板没有命名空间；扩展模板不参加无前缀调用的匹配。

## 普通调用匹配

匹配器处理解析后的语法结构，不直接拿固定文字正则切原始源码。每个候选都要消费整段积木内容，再比较结果。

1. 根据位置选择类型。文件或角色的声明体允许帽子，脚本体允许命令与内置控制流，参数位置允许值。帽子要结合 `header { body }` 识别，不能只看前缀后的第一个括号：例如 `[VALUE] 达标` 帽子的调用 `evt.(x 坐标) 达标 { [等待 1 秒] }` 中，圆括号是帽子参数。帽子不能放在值或运行语句位置。
2. 确定可见模板。有前缀时只查该命名空间，未知名称报错；无前缀时查内置和可见自制积木。私有过程只遮蔽同签名的公共过程，其他签名仍一起匹配。公共过程体按定义处的全局作用域解析。
3. 将字符串、菜单、变量/列表/参数引用及嵌套积木作为参数原子。`"inner".(x 坐标)`、`inner.(x 坐标)` 的前缀、点和括号体不能拆开，显式 opcode 调用也一样。若需要字符串 `"inner"` 后跟外层固定文字 `.`，两者之间须用语法空白隔开。
4. 依次匹配 `tokens`。固定文字消费文字，槽位消费一个类型合法的值，两端都要对齐。模板可以从槽位开始或结束，可以有相邻槽位，也可以全是槽位。相邻参数须有空白，或前一原子有闭合分隔符且后一原子能独立起始。不能把 `123` 拆成 `1` 和 `23`，不能把 `1-2` 当作两个数字；`"a""b"` 则是两个有分隔的字符串。
5. 每个候选、每层表达式独立记录解析状态。进入槽位时从读取值开始；二元运算读完左操作数后，才读取运算符。`<`、`>` 只有在左右操作数及当前组边界都成立时才按比较运算符解析；其他位置按布尔积木边界处理。`/` 要区分除法和菜单的开始，解析会检查左右值及当前积木的完整边界，不能把上个槽位的状态带进下一个槽位。菜单 `/'*'/` 表示星号，`/''/` 表示空菜单。一个内置运算积木只含一个运算符，复合运算用嵌套积木，不设隐式优先级或额外的数学分组括号。
6. 注释按词法规则转为空白并保留换行。同一行有后续 `#` 时结束行内块；否则单独的 `#` 行开启跨行块，由下一个 `#` 结束；其他情况注释到行尾。块不嵌套，未闭合报错。字符串、引号命名空间、菜单里的 `#` 是内容。语句层换行分隔语句，括号内换行作为语法空白。原子内部和注释内容不参与外层固定文字匹配。
7. 排除缺参、剩余文字、类型不符或参数解析失败的候选。剩余候选按归一化后固定文字的 Unicode 码点总数选最长者，计入归一化空格，不计前缀和参数。最长者不唯一，或选中模板仍有多种参数切分时，报告歧义及候选 opcode。注册顺序不参与选择。同一 opcode 的内置别名得到相同槽位绑定时可以合并。

例如 `读取 [A]` 与 `读取 [A].[B]` 并存时，`outer.[读取 "inner".(x 坐标)]` 的内层调用只能作为一个参数，不能为了匹配较长模板而拆成字符串、点和原版报告积木。若固定文字使用保留分隔符，普通形式无法无歧义书写，就改用显式 opcode；固定文字没有另一套反斜杠转义。

## 显式 opcode 匹配

[显式调用](./extensions.md#指定-opcode)只用于第三方扩展。先用命名空间和 `local_opcode` 精确选择模板，再按槽位顺序读取逗号分隔的参数，跳过显示文字匹配。同一个扩展内两个 opcode 都显示“清空”时，可分别用 `ext::"clearA".[]`、`ext::"clearB".[]` 指定。

opcode 使用双引号字符串，不含扩展 ID，遵循普通字符串转义。命名空间、`::`、opcode、`.`、形状起始必须连续，帽子固定写 `hat(`。参数不能省略、带尾逗号或夹入显示文字；零参数时为空。嵌套原子里的逗号不分隔外层参数，菜单仍按菜单规则处理。

选中模板后照常检查形状、参数数量、类型和目标限制；缺少元信息、未知 opcode、未支持类型或运行时 ID 冲突均报错，不猜测参数顺序。
