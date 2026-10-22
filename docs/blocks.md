# 内置积木

这些模板已注册在 [`compiler/src/builtins.rs`](../compiler/src/builtins.rs) 中。控制流另由解析器处理，写法见[语法](./syntax.md)。

表中大写名称是占位符，调用时替换成实际内容，保留槽位括号。例如 `移动(STEPS)步` 写成 `移动(10)步`；接受报告积木的菜单槽 `('选项')` 使用圆括号，具体如 `移到('鼠标指针')`，不接受报告积木的菜单字段如 `停止'全部脚本'` 直接使用单引号。布尔槽使用尖括号，可填布尔表达式或空槽 `<>`。广播接收事件的名称字段直接写双引号字符串，例如 `当收到广播 "跳跃":`；发送广播的输入则写 `广播("跳跃")`。

这些对照表只列显示文字和 opcode。生成 `.sb3` 所需的字段名、shadow、符号类别和适用目标，以注册表中的槽位定义为准。

## 运动 (`motion_*`)

| 模板 | opcode |
| --- | --- |
| `移动 (STEPS) 步` | `motion_movesteps` |
| `右转 (DEGREES) 度` | `motion_turnright` |
| `左转 (DEGREES) 度` | `motion_turnleft` |
| `移到 x: (X) y: (Y)` | `motion_gotoxy` |
| `移到 ('TO')` | `motion_goto` (`menu: motion_goto_menu`) |
| `在 (SECS) 秒内滑行到 x: (X) y: (Y)` | `motion_glidesecstoxy` |
| `面向 (DIRECTION) 方向` | `motion_pointindirection` |
| `将 x 坐标增加 (DX)` / `将 x 坐标设为 (X)` | `motion_changexby` / `motion_setx` |
| `将 y 坐标增加 (DY)` / `将 y 坐标设为 (Y)` | `motion_changeyby` / `motion_sety` |
| `碰到边缘就反弹` | `motion_ifonedgebounce` |
| `(x 坐标)` / `(y 坐标)` / `(方向)` | `motion_xposition` / `motion_yposition` / `motion_direction` |

## 外观 (`looks_*`)

| 模板 | opcode |
| --- | --- |
| `说 (MESSAGE) (SECS) 秒` | `looks_sayforsecs` |
| `说 (MESSAGE)` | `looks_say` |
| `思考 (MESSAGE) (SECS) 秒` | `looks_thinkforsecs` |
| `思考 (MESSAGE)` | `looks_think` |
| `换成 ('COSTUME') 造型` | `looks_switchcostumeto` |
| `下一个造型` | `looks_nextcostume` |
| `换成 ('BACKDROP') 背景` | `looks_switchbackdropto` |
| `将大小增加 (CHANGE)` / `将大小设为 (SIZE) %` | `looks_changesizeby` / `looks_setsizeto` |
| `显示` / `隐藏` | `looks_show` / `looks_hide` |
| `(大小)` | `looks_size` |

## 事件与控制 (`event_*` / `control_*`)

| 模板 | opcode |
| --- | --- |
| `start` | `event_whenflagclicked` |
| `loop N:` | `control_repeat` |
| `while <条件>:` | `control_while` |
| `while true:` | `control_forever` |
| `if <条件>:` / `elif <条件>:` / `else:` | `control_if` / `control_if_else`（`elif` 嵌套到否则体） |
| `当角色被点击` | `event_whenthisspriteclicked` |
| `当按下 'KEY_OPTION' 键` | `event_whenkeypressed` |
| `当收到广播 "BROADCAST_OPTION"` | `event_whenbroadcastreceived` |
| `广播 (BROADCAST_INPUT)` | `event_broadcast` |
| `广播 (BROADCAST_INPUT) 并等待` | `event_broadcastandwait` |
| `等待 (DURATION) 秒` | `control_wait` |
| `等待直到 <CONDITION>` | `control_wait_until` |
| `停止 'STOP_OPTION'` | `control_stop` |
| `当作为克隆体启动时` | `control_start_as_clone` |
| `克隆 ('CLONE_OPTION')` | `control_create_clone_of` |
| `删除此克隆体` | `control_delete_this_clone` |

## 侦测 (`sensing_*`)

| 模板 | opcode |
| --- | --- |
| `<碰到 ('TOUCHINGOBJECTMENU') ?>` | `sensing_touchingobject` |
| `<按下 ('KEY_OPTION') 键?>` | `sensing_keypressed` |
| `<鼠标按下?>` | `sensing_mousedown` |
| `(到 ('DISTANCETOMENU') 的距离)` | `sensing_distanceto` |
| `询问 (QUESTION) 并等待` | `sensing_askandwait` |
| `(回答)` | `sensing_answer` |
| `(鼠标的 x 坐标)` / `(鼠标的 y 坐标)` | `sensing_mousex` / `sensing_mousey` |
| `(计时器)` / `计时器归零` | `sensing_timer` / `sensing_resettimer` |

## 运算 (`operator_*`)

| 模板 | opcode |
| --- | --- |
| `(A + B)` | `operator_add` |
| `(A - B)` | `operator_subtract` |
| `(A * B)` | `operator_multiply` |
| `(A / B)` | `operator_divide` |
| `(A % B)` / `((A) 除以 (B) 的余数)` | `operator_mod` |
| `(在 (FROM) 和 (TO) 之间取随机数)` | `operator_random` |
| `<A > B>` / `<A < B>` / `<A = B>` | `operator_gt` / `operator_lt` / `operator_equals` |
| `<<A> and <B>>` / `<<A> or <B>>` / `<not <A>>` | `operator_and` / `operator_or` / `operator_not` |
| `(连接 (STRING1) 和 (STRING2))` | `operator_join` |
| `((STRING) 的第 (LETTER) 个字符)` | `operator_letter_of` |
| `((STRING) 的字符数)` | `operator_length` |
| `<(STRING1) 包含 (STRING2) ?>` | `operator_contains` |
| `(四舍五入 (NUM))` | `operator_round` |
| `('OPERATOR' (NUM))` | `operator_mathop` |

## 变量与列表 (`data_*`)

| 模板 | opcode |
| --- | --- |
| `VARIABLE = VALUE` | `data_setvariableto` |
| `VARIABLE += VALUE` | `data_changevariableby` |
| `显示变量 (VARIABLE)` / `隐藏变量 (VARIABLE)` | `data_showvariable` / `data_hidevariable` |
| `将 (ITEM) 加入 (LIST)` | `data_addtolist` |
| `删除 (LIST) 的第 (INDEX) 项` | `data_deleteoflist` |
| `删除 (LIST) 的全部项目` | `data_deletealloflist` |
| `在 (LIST) 的第 (INDEX) 项前插入 (ITEM)` | `data_insertatlist` |
| `将 (LIST) 的第 (INDEX) 项替换为 (ITEM)` | `data_replaceitemoflist` |
| `((LIST) 的第 (INDEX) 项)` | `data_itemoflist` |
| `((LIST) 中第一个 (ITEM) 的编号)` | `data_itemnumoflist` |
| `((LIST) 的项目数)` | `data_lengthoflist` |
| `<(LIST) 包含 (ITEM) ?>` | `data_listcontainsitem` |

变量赋值是独立语句，例如 `score = 10`、`score += 1`，不包方括号。还支持 `-=`、`*=`、`/=`、`%=`，会展开为对应的变量与运算积木；列表赋值 `logs = {"a", 1}` 在脚本中先清空再加入各字面量元素；数据种类由首次赋值确定，不能互转。其他数据操作使用表中的裸命令与带括号输入。
