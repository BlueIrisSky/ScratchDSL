# SB3 生成

[`codegen.rs`](../compiler/src/codegen.rs) 将绑定后的程序转换为 Scratch 积木图，[`build.rs`](../compiler/src/build.rs) 处理造型、声音和扩展源码，最后把 `project.json` 与资源写入 `.sb3`。

## 输入槽位

槽位先通过类型检查，再按模板的 `Shadow`（`opcode`、`field`、`primitive`）编码。shadow 类型由目标槽位决定，不能只看源码字面量的类型。

| 槽位或 shadow | primitive tag | 字面量输入 |
| --- | --- | --- |
| `string` / `text` | `10` | `[1, [10, "hello"]]` |
| `number` / `math_number` | `4` | `[1, [4, "42"]]` |
| `math_positive_number`、`math_whole_number`、`math_integer` | `5`、`6`、`7` | 保留内置模板指定的 tag |
| `angle` / `math_angle` | `8` | `[1, [8, "90"]]` |
| `color` / `colour_picker` | `9` | `[1, [9, "#ff0000"]]` |
| `Boolean` | 无 | 引用布尔积木 |

普通输入有三种连接形式：

| 情况 | 编码 |
| --- | --- |
| 只有 shadow | `[1, shadowDesc]` |
| 子积木覆盖 shadow | `[3, childBlockId, shadowDesc]` |
| 没有 shadow | `[2, childBlockId]` |

`shadowDesc` 可以是表中的压缩 primitive，也可以是独立 shadow 块的 ID。被覆盖的 shadow 保留槽位默认值；未指定默认值时，普通字符串用 `""`，数值和角度用 `0`，颜色用 `"#000000"`。菜单按下一节处理。

变量值编码为 `[12, varName, varId]`，列表值为 `[13, listName, listId]`。它们可以代替上表中的子积木 ID，例如数值输入中的变量是 `[3, [12, varName, varId], [4, "0"]]`。ID 来自绑定结果，不能用显示名称代替。

布尔输入使用 `[2, boolBlockId]`。`<true>` 和 `<false>` 生成比较积木，再引用其 ID。空槽 `<>` 是专门的 AST 标记，序列化时省略对应的 `inputs` 项；它不表示 `false`，也没有 text shadow。显式调用 `ext::"ready".<>` 的空参数区则表示零参数布尔积木。

内置控制积木的 `SUBSTACK` 为 `[2, firstSubBlockId]`，否则分支写入 `SUBSTACK2`；空子栈为 `[2, null]`。它们由专门的 AST 节点生成，扩展 `conditional` 和 `loop` 尚未支持。

参数子块、shadow 和子栈首块的 `parent` 指向所属块。栈中的后续语句以之前一块为 `parent`，按顺序连接 `next`，最后一块的 `next` 为 `null`。压缩 primitive 的父关系由输入位置表示。

对应格式可对照 [TurboWarp 的 SB3 序列化代码](https://github.com/TurboWarp/scratch-vm/blob/develop/src/serialization/sb3.js)。

## 菜单与字段

静态菜单保留有序的 `text`、`value` 对。菜单字面量先按精确 `value` 匹配，再按唯一的 `text` 别名匹配；别名不唯一时报错。AST 保存解析后的 `value`，代码生成只写该值。菜单源码统一写成 `/'选项'/`，单引号与外层斜杠不写入项目。

### 扩展菜单

`acceptReporters: false` 时，参数存入父块的 `fields[ARG_NAME] = [selectedValue, null]`。不会创建菜单 shadow，也不接受报告积木。参数 `MODE` 即使关联的菜单名是 `modes`，字段名仍然是 `MODE`。

`acceptReporters: true` 时，菜单 shadow 的 opcode 是 `${extensionId}_menu_${menuName}`，字段名是 `menuName`：

```text
next: null
parent: parentBlockId
inputs: {}
fields: { [menuName]: [selectedValue, null] }
shadow: true
topLevel: false
```

菜单字面量对应 `inputs[ARG_NAME] = [1, menuBlockId]`，报告积木覆盖它时为 `[3, childBlockId, menuBlockId]`。这个槽位也接受字符串、数字、变量值和非空布尔表达式，不检查这些表达式是否属于静态菜单。例如字符串可编码为 `[3, [10, "任意文本"], menuBlockId]`，数字为 `[3, [4, "42"], menuBlockId]`。这些值是覆盖菜单的子值，不写入静态下拉字段。空布尔槽 `<>` 不能用在这里。

扩展元信息用方法名指定动态菜单，模板验证后将其标记为动态菜单，不保存或调用该方法。源码给出的菜单值直接作为 `value`，不检查静态成员。

需要默认 shadow 时，优先使用明确的 `defaultValue`，否则静态菜单取第一项的 `value`。动态菜单没有默认值时，报告积木和普通子值使用 `[2, childDesc]`，不猜测菜单选项。源码中的菜单字面量本身已有值，可直接生成 shadow；源码参数仍不能省略。

菜单格式分别对应 [占位符转换](https://github.com/TurboWarp/scratch-vm/blob/develop/src/engine/runtime.js)与[扩展菜单元数据处理](https://github.com/TurboWarp/scratch-vm/blob/develop/src/extension-support/extension-manager.js)。

### 内置字段

内置菜单使用模板明确给出的存储位置、shadow opcode 和字段名，不套用扩展命名公式。例如 `event_whenkeypressed` 写父块的 `fields.KEY_OPTION`；`motion_goto` 使用 `motion_goto_menu` shadow，字段为 `TO`。

变量操作目标写 `fields.VARIABLE = [varName, varId]`，列表操作目标写 `fields.LIST = [listName, listId]`。两者都使用已绑定的符号 ID。

广播统一在 Stage 的 `broadcasts` 表中保存 `broadcastId → broadcastName`。接收广播帽子写 `fields.BROADCAST_OPTION = [broadcastName, broadcastId]`；发送广播的菜单值为 `[1, [11, broadcastName, broadcastId]]`。表达式覆盖广播菜单时，保留这个 shadow，按普通输入关系连接。广播是内置模板的符号引用特例，第三方参数类型无需增加广播类型。

## 自制积木

绑定前，`@stage { ... }` 中的声明按源位置展开，与该位置的顶层声明等价。过程签名由规范化空白后的固定文本和有序槽位类型组成，参数名不参与签名。同一作用域出现重复签名时报错。

过程体中的调用和变量引用按定义处的词法作用域绑定。公共过程复制到角色之后，内部调用仍指向原来绑定的过程，不会被角色的同签名私有过程替换。

每个目标，包括 Stage，都沿绑定后的调用图收集需要的公共过程，并为各过程生成一个目标实例。递归和互递归通过已分配的源符号、实例表终止遍历。实例化时检查过程体是否适用于目标，例如 Stage 不能调用包含仅角色可用积木的公共过程。

源过程有稳定的 `symbolId`，目标实例根据目标和源符号生成内部 ID。同一个目标中的不同源符号拥有不同的 `proccode`，因此公共过程和同签名私有过程可以共存。当前生成器在显示文本后加上 `[sbdsl:…]` 标记，并把源固定文字中的 `%` 换成 `％`，防止 `%s`、`%b`、`%n` 被 Scratch 当成参数。实际参数才生成占位符：普通参数 `%s`，布尔参数 `%b`。

调用直接使用绑定结果对应的 `proccode`，不会在代码生成阶段重新按显示文字查找。Scratch 在目标内部用 `proccode` 识别过程。

| 积木 | 生成内容 |
| --- | --- |
| `procedures_definition` | `topLevel: true`、`parent: null`；`inputs.custom_block = [1, prototypeBlockId]`；`next` 指向过程体首块 |
| `procedures_prototype` | `shadow: true`、`parent: definitionBlockId`；mutation 含过程及参数信息 |
| `procedures_call` | mutation 使用目标实例的 `proccode`、`argumentids`、`warp`；`inputs` 用参数 ID 作键 |

原型 mutation 包含 `tagName: "mutation"`、`children: []`、`proccode`、`argumentids`、`argumentnames`、`argumentdefaults` 和 `warp`。三个参数数组都保存为 JSON 字符串，`warp` 为字符串 `"true"` 或 `"false"`。

原型的每个参数还会生成 `argument_reporter_string_number` 或 `argument_reporter_boolean` shadow，写入 `fields.VALUE = [parameterName, null]`，以原型为 `parent`。原型通过 `inputs[argumentId] = [1, argumentReporterId]` 引用它。过程体使用参数时，另外生成普通参数报告积木；布尔参数按布尔输入连接。

`project.json.meta.scratchDSL` 保存带 `version` 的来源信息：源 `symbolId`、原始模板文本、参数类型和名称、作用域、源位置、绑定关系，以及每个目标实例的 `proccode` 和参数 ID。目标键区分 Stage 和角色。这些数据用于核对源定义与目标实例，运行时身份仍由 `proccode` 决定。

相关实现见 VM 的[过程查找](https://github.com/TurboWarp/scratch-vm/blob/develop/src/engine/blocks.js)和 Scratch Blocks 的[原型及参数积木](https://github.com/TurboWarp/scratch-blocks/blob/develop/blocks_vertical/procedures.js)。

## 静态属性、画布与资源

顶层事件和 `procedures_definition` 的 `topLevel` 为 `true`，`parent` 为 `null`。画布按脚本顺序排成四列，坐标为 `x = 40 + (index % 4) * 350`、`y = 40 + (index / 4) * 250`，其中 `index / 4` 取整。

角色的 `@init` 写入 target 的 `x`、`y`、`size`、`direction`、`visible`、`rotationStyle`、`costumes` 和 `sounds`，不生成绿旗初始化脚本。`currentCostume` 为 `0`，选中第一项造型；造型列表为空时使用默认空白造型。字段类型和默认值见[角色](./sprites.md)。

角色资源路径相对于 `@init` 所在文件，导入模块时保留该来源。舞台资源来自 `sbdsl.buildsb.stage`，相对于配置文件。两者均拒绝 URL；`sbdsl.buildsb` 中的 `sprites` 按未知字段报错。

造型支持 SVG、PNG，音频支持 WAV、MP3。资源按实际字节计算 MD5，以 `<md5>.<ext>` 保存；默认空白 SVG 也执行相同处理。`assetId`、`md5ext` 和 ZIP 内的文件名必须一致。

`zip` crate 将 `project.json` 和资源打包为 `.sb3`，使用目标运行时支持的标准 ZIP 存储或 Deflate 压缩。JSON 由 `serde` / `serde_json` 读写。

## 扩展身份与源码

编译器只读取 `sbdsl.buildsb.extensions` 中的本地 `.js` 文件，如 `./extensions/自定义拓展.js`。源码 namespace 默认来自文件名，也可以用 `as "别名"` 指定；`getInfo().id` 决定运行时扩展 ID。注册时建立两者的映射，检查本地路径重复、namespace 冲突、扩展 ID 冲突和真实 opcode 冲突，规则见[扩展](./extensions.md)。

普通调用和[显式 opcode 调用](extensions.md#指定-opcode)经过模板校验后，都会保留 namespace、扩展 ID、完整 opcode 和归一化参数。文件 `自定义拓展.js` 的 ID 若为 `custom`，其 `execute` 积木的真实 opcode 就是 `custom_execute`，代码生成直接使用此值。

`project.json.extensions` 和 `extensionURLs` 的键使用真实扩展 ID。所有配置扩展的源码都内联为 `data:application/javascript;charset=utf-8;base64,...`，写入 `extensionURLs[extensionId]`。该字段不写远程地址或本地文件路径，也没有关闭内联的选项。字段格式见 [TurboWarp 序列化代码](https://github.com/TurboWarp/scratch-vm/blob/develop/src/serialization/sb3.js)。

支持的参数类型为 `string`、`number`、`Boolean`、`color`、`angle` 和模板内部的变量、列表引用。`matrix`、`note`、`image`、扩展 `conditional` / `loop`、多段 `text` 等尚未支持，遇到时明确报错。label、button、separator 等纯 GUI 项略过。显式 opcode 调用也遵循这些限制，细节见[模板实现](./templates.md)。

## TurboWarp 运行时设置

配置了 `sbdsl.buildsb.runtime` 时，在 Stage 的 `comments.sbdsl_runtime` 写入折叠的独立注释，`blockId: null`、`minimized: true`。文本第一行是 `ScratchDSL TurboWarp settings`，第二行为单行 JSON，末尾紧接 ` // _twconfig_`。

| 配置字段 | 注释 JSON 的键 |
| --- | --- |
| `fps` | `framerate` |
| `stageWidth` / `stageHeight` | `width` / `height` |
| `hqPen` | `hq` |
| `maxClones` | `runtimeOptions.maxClones` |
| `fencing` | `runtimeOptions.fencing` |
| `miscLimits` | `runtimeOptions.miscLimits` |

未配置的键省略；`runtimeOptions` 仅在含有子字段时写入。例如：

```text
ScratchDSL TurboWarp settings
{"framerate":60,"width":640,"height":360,"hq":true,"runtimeOptions":{"maxClones":1000,"fencing":false,"miscLimits":false}} // _twconfig_
```

`// _twconfig_` 是 JSON 之后的格式标记，不是 JSON 字段，也不是 `.sbdsl` 的注释语法。设置是否生效由打开项目的运行环境决定。
