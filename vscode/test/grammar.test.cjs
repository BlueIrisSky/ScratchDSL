const { test, before } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const tm = require('vscode-textmate');
const onig = require('vscode-oniguruma');

let grammar;
let buildGrammar;
function createRegistry(theme) {
  return new tm.Registry({
    theme,
    onigLib: Promise.resolve({
      createOnigScanner: patterns => new onig.OnigScanner(patterns),
      createOnigString: text => new onig.OnigString(text)
    }),
    loadGrammar: async scope => {
      const filename = {
        'source.sbdsl': 'sbdsl.tmLanguage.json',
        'source.sbdsl-build': 'buildsb.tmLanguage.json'
      }[scope];
      return filename ? tm.parseRawGrammar(
        fs.readFileSync(path.join(__dirname, '../syntaxes', filename), 'utf8'), filename
      ) : null;
    }
  });
}
before(async () => {
  const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
  await onig.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = createRegistry();
  grammar = await registry.loadGrammar('source.sbdsl');
  buildGrammar = await registry.loadGrammar('source.sbdsl-build');
});

function tokenize(source, language = grammar) {
  let stack = tm.INITIAL;
  return source.split('\n').map(line => {
    const result = language.tokenizeLine(line, stack);
    stack = result.ruleStack;
    return { line, tokens: result.tokens };
  });
}
function scopes(row, text, occurrence = 0) {
  let position = -1;
  for (let i = 0; i <= occurrence; i++) position = row.line.indexOf(text, position + 1);
  assert(position >= 0, 'missing ' + text + ' in ' + row.line);
  return row.tokens.find(token => token.startIndex <= position && token.endIndex > position).scopes;
}
function scopesAt(row, position) {
  return row.tokens.find(token => token.startIndex <= position && token.endIndex > position).scopes;
}
function menu(row, text, occurrence = 0) {
  let start = -1;
  for (let index = 0; index <= occurrence; index++) start = row.line.indexOf(text, start + 1);
  assert(start >= 0, 'missing ' + text + ' in ' + row.line);
  assert(text.startsWith("/'") && text.endsWith("'/"), 'complete menu: ' + text);
  for (const [position, boundary] of [[start, 'begin'], [start + text.length - 1, 'end']]) {
    const scopes = scopesAt(row, position);
    assert(scopes.includes('meta.menu.sbdsl'), text + ': ' + scopes);
    assert(scopes.includes(`punctuation.definition.menu.${boundary}.sbdsl`), text + ': ' + scopes);
    assert(!scopes.some(scope => scope.startsWith('string.')), 'menu slash inherits a string: ' + scopes);
  }
  for (let position = start + 1; position < start + text.length - 1; position++) {
    const scopes = scopesAt(row, position);
    assert(scopes.includes('string.quoted.other.menu.sbdsl'), text + ': ' + scopes);
    assert(!scopes.some(scope => scope.startsWith('punctuation.definition.menu.')), text + ': ' + scopes);
  }
}
function has(row, text, scope, occurrence = 0) {
  assert(scopes(row, text, occurrence).includes(scope), text + ': ' + scopes(row, text, occurrence));
}
function lacks(row, text, prefix) {
  assert(!scopes(row, text).some(scope => scope.startsWith(prefix)), text + ': ' + scopes(row, text));
}

test('hash comments distinguish line, inline and multiline blocks', () => {
  const lines = tokenize('# 行注释\n[说 1] # 中间 # [说 2]\n  #  \n多行\n结束 # [说 3]\n[说 "#字符串"]');
  has(lines[0], '行注释', 'comment.line.number-sign.sbdsl');
  has(lines[1], '中间', 'comment.block.sbdsl');
  lacks(lines[1], '2', 'comment.');
  has(lines[3], '多行', 'comment.block.sbdsl');
  lacks(lines[4], '3', 'comment.');
  has(lines[5], '#字符串', 'string.quoted.double.sbdsl');
});

test('menus retain hashes, stars, empty values and quoted slash content', () => {
  const [line] = tokenize("[说 /'选项'/] [说 /''/] [说 /'*'/] [说 /'#'/]\n");
  for (const value of ["/'选项'/", "/''/", "/'*'/", "/'#'/"]) menu(line, value);
  const [quoted] = tokenize("\"自定义拓展\".[模式 /'a/b\"#'/]");
  has(quoted, 'a/b', 'string.quoted.other.menu.sbdsl');
  lacks(quoted, '#', 'comment.');
  has(quoted, '"自定义拓展"', 'entity.name.namespace.sbdsl');
});

test('menu delimiters are punctuation while quoted slashes and quotes remain strings', () => {
  for (const value of ["/'梯度'/", "/''/", "/'*'/", "/'/'/", "/'a/b'/", "/'a\"/#'/"]) {
    const [line] = tokenize('"scratch-tensor-extension".(获取 Tensor "W" 的 ' + value + ')');
    menu(line, value);
    has(line, ')', 'punctuation.definition.reporter.end.sbdsl');
    if (value.startsWith("/'")) {
      const start = line.line.indexOf(value);
      assert(scopesAt(line, start + 1).includes('punctuation.definition.string.begin.sbdsl'));
      assert(scopesAt(line, start + value.length - 2).includes('punctuation.definition.string.end.sbdsl'));
    }
  }
  const [literalSlash] = tokenize("[说 /'/'/]");
  has(literalSlash, '/', 'string.quoted.single.menu.sbdsl', 1);
  assert(!scopes(literalSlash, '/', 1).some(scope => scope.startsWith('punctuation.definition.menu.')));
});

test('menus require single quotes and use JSON escapes plus escaped apostrophes', () => {
  const value = String.raw`/'a\'b\n\r\t\b\f\u732b\/\\\"#()<>/"'/`;
  const [line] = tokenize('[说 ' + value + '] [移动 1 步]');
  menu(line, value);
  for (const escape of [String.raw`\'`, String.raw`\n`, String.raw`\r`, String.raw`\t`,
    String.raw`\b`, String.raw`\f`, String.raw`\u732b`, String.raw`\/`, String.raw`\\`, String.raw`\"`]) {
    has(line, escape, 'constant.character.escape.sbdsl');
  }
  lacks(line, '#', 'comment.');
  has(line, '移动', 'support.function.command.sbdsl');
  for (const legacy of ['/space/', '//', '/*/', '/"space"/']) {
    const [row] = tokenize('[说 ' + legacy + ']');
    assert(!row.tokens.some(token => token.scopes.includes('meta.menu.sbdsl')), legacy);
  }
  const [invalid] = tokenize(String.raw`[说 /'bad\q'/]`);
  has(invalid, String.raw`\q`, 'invalid.illegal.escape.sbdsl');
});

test('logical words in command and reporter template text keep the surrounding block scopes', () => {
  const [command] = tokenize("\"scratch-tensor-extension\".[张量 \"A\" 与 \"B\" 进行 /'*'/ 运算 赋值给 \"product\"]");
  for (const text of ['张量', '与', '进行', '运算', '赋值给']) {
    has(command, text, 'support.function.command.sbdsl');
    lacks(command, text, 'keyword.operator.logical.');
  }
  menu(command, "/'*'/");
  const [ordinary] = tokenize('[选择 "A" 或 "B" 不成立] (连接 "A" 与 "B" 或 "C" 不成立)');
  for (const text of ['或', '不成立']) {
    has(ordinary, text, 'support.function.command.sbdsl');
    lacks(ordinary, text, 'keyword.operator.logical.');
    has(ordinary, text, 'support.function.reporter.sbdsl', 1);
  }
  has(ordinary, '与', 'support.function.reporter.sbdsl');
  lacks(ordinary, '与', 'keyword.operator.logical.');
});

test('logical operators are highlighted within boolean expressions and stay isolated from literals', () => {
  const [line] = tokenize('[说 <<true> 与 <不成立 <false>>>] [说 <<false> 或 <true>>]');
  for (const text of ['与', '或', '不成立']) {
    has(line, text, 'keyword.operator.logical.sbdsl');
    has(line, text, 'meta.block.boolean.sbdsl');
    lacks(line, text, 'support.function.command.');
    lacks(line, text, 'support.function.reporter.');
  }
  const [literal] = tokenize("[说 \"与 或 不成立\"] [说 /'与 或 不成立'/] <与会者>");
  for (const text of ['与', '或', '不成立']) {
    has(literal, text, 'string.quoted.double.sbdsl');
    has(literal, text, 'string.quoted.other.menu.sbdsl', 1);
  }
  menu(literal, "/'与 或 不成立'/");
  has(literal, '与会者', 'support.function.boolean.sbdsl');
  lacks(literal, '与会者', 'keyword.operator.logical.');
});

test('division and comparison operators do not start menus or boolean brackets', () => {
  const [line] = tokenize("(8 / (6 / 2)) <$hp < 1> ($hp / 2) ((\"x\") / 2)");
  has(line, '/', 'keyword.operator.sbdsl');
  has(line, '/', 'keyword.operator.sbdsl', 1);
  has(line, "<", 'keyword.operator.sbdsl', 1);
  has(line, '/', 'keyword.operator.sbdsl', 2);
  has(line, '/', 'keyword.operator.sbdsl', 3);
  const [menuLine] = tokenize("[说 /'* abc'/]");
  menu(menuLine, "/'* abc'/");
  const [adjacentSlots] = tokenize("[自定义 8 /'*'/]");
  menu(adjacentSlots, "/'*'/");
});

test('init shares build syntax with bare keys and JSON-only escapes', () => {
  const lines = tokenize([
    '@init {',
    '  costumes: [{name:"\\u732b\\t\\/#", path:"../assets/a.svg"}]',
    '  visible: false',
    '}',
    '[说 "\\t"] # DSL escape is invalid'
  ].join('\n'));
  has(lines[0], '@init', 'keyword.control.sbdsl');
  has(lines[1], 'costumes', 'support.type.property-name.json');
  has(lines[1], 'name', 'support.type.property-name.json');
  has(lines[1], '\\u732b', 'constant.character.escape.json');
  has(lines[1], '\\t', 'constant.character.escape.json');
  lacks(lines[1], '#', 'comment.');
  has(lines[2], 'false', 'constant.language.json');
  has(lines[4], '\\t', 'invalid.illegal.escape.sbdsl');
  lacks(lines[4], '说', 'meta.embedded.block.buildsb');
});

test('language configuration avoids ambiguous automatic slash/angle/hash insertion', () => {
  const config = require('../language-configuration.json');
  assert.equal(config.comments, undefined);
  assert(!config.autoClosingPairs.some(pair => ['/', '<'].includes(pair.open)));
});

test('all fixed block text is highlighted while nested values keep their own scopes', () => {
  const [line] = tokenize("[将 $score 设为 (连接 \"分数\" 和 ($list:记录 的第 1 项))] <碰到 /'边缘'/ ?>");
  for (const text of ['将', '设为']) has(line, text, 'support.function.command.sbdsl');
  for (const text of ['连接', '和', '的第', '项']) has(line, text, 'support.function.reporter.sbdsl');
  for (const text of ['碰到', '?']) has(line, text, 'support.function.boolean.sbdsl');
  has(line, '$score', 'variable.other.readwrite.sbdsl');
  has(line, '$list:记录', 'variable.other.array.sbdsl');
  has(line, '分数', 'string.quoted.double.sbdsl');
  menu(line, "/'边缘'/");
  has(line, '1', 'constant.numeric.sbdsl');
  lacks(line, '分数', 'support.function.');
});

test('declarations identify role, variable, list and module names including quoted names', () => {
  const lines = tokenize([
    'import lib.动作',
    'import "./模块/移动.sbdsl"',
    '@sprite "玩家 🐱" add {',
    '  @global @var 分数１ = -90',
    '  @var "姓名 🐱" = "玩家"',
    '  @list "任务 列表" = ["开始", 1]',
    '}'
  ].join('\n'));
  has(lines[0], 'import', 'keyword.control.import.sbdsl');
  has(lines[0], 'lib.动作', 'entity.name.namespace.module.sbdsl');
  has(lines[1], './模块/移动.sbdsl', 'entity.name.namespace.module.sbdsl');
  has(lines[2], '玩家 🐱', 'entity.name.type.sprite.sbdsl');
  has(lines[2], 'add', 'keyword.control.sbdsl');
  has(lines[3], '@global', 'storage.modifier.sbdsl');
  has(lines[3], '@var', 'storage.type.sbdsl');
  has(lines[3], '分数１', 'variable.other.readwrite.sbdsl');
  has(lines[3], '-90', 'constant.numeric.sbdsl');
  has(lines[4], '姓名 🐱', 'variable.other.readwrite.sbdsl');
  has(lines[4], '玩家', 'string.quoted.double.sbdsl');
  has(lines[5], '任务 列表', 'variable.other.array.sbdsl');
  has(lines[5], '开始', 'string.quoted.double.sbdsl');
  lacks(lines[5], '开始', 'meta.block.command.');
});

test('references distinguish lists, variables and both parameter shapes', () => {
  const [line] = tokenize('[使用 $hp $global:hp $list:记录 $global:list:"任务 🐱" @步数 @"用户 名" <@条件> <@"可以 开始">]');
  for (const text of ['$hp', '$global:hp']) has(line, text, 'variable.other.readwrite.sbdsl');
  for (const text of ['$list:记录', '$global:list:"任务 🐱"']) has(line, text, 'variable.other.array.sbdsl');
  for (const text of ['@步数', '@"用户 名"']) has(line, text, 'variable.parameter.sbdsl');
  for (const text of ['@条件', '@"可以 开始"']) has(line, text, 'variable.parameter.boolean.sbdsl');
  const namedLikeDirective = tokenize('定义自制积木 播报 @init @var {\n[说\n @init\n]\n[说 @var]\n}');
  has(namedLikeDirective[0], '@init', 'variable.parameter.sbdsl');
  has(namedLikeDirective[0], '@var', 'variable.parameter.sbdsl');
  has(namedLikeDirective[2], '@init', 'variable.parameter.sbdsl');
  has(namedLikeDirective[2], '@init', 'meta.block.command.sbdsl');
  has(namedLikeDirective[4], '@var', 'variable.parameter.sbdsl');
});

test('procedure templates, events and control flow highlight complete fixed text', () => {
  const lines = tokenize([
    '定义自制积木 移动并播报 步数: @steps 是否说话: <@say> @warp {',
    '  如果 <@say> 那么 { [说 @steps] }',
    '  否则 { [移动 @steps 步] }',
    '}',
    '当收到广播 "开始" {',
    '  循环 2 次 { [移动并播报 步数: 10 是否说话: <true>] }',
    '}',
    '当作为克隆体启动时 { [隐藏] }'
  ].join('\n'));
  has(lines[0], '定义自制积木', 'keyword.declaration.function.sbdsl');
  for (const text of ['移动并播报', '步数:', '是否说话:']) has(lines[0], text, 'entity.name.function.sbdsl');
  has(lines[0], '@steps', 'variable.parameter.sbdsl');
  has(lines[0], '@say', 'variable.parameter.boolean.sbdsl');
  has(lines[0], '@warp', 'storage.modifier.sbdsl');
  for (const text of ['如果', '那么']) has(lines[1], text, 'keyword.control.sbdsl');
  has(lines[2], '否则', 'keyword.control.sbdsl');
  has(lines[4], '当收到广播', 'keyword.control.event.sbdsl');
  has(lines[4], '开始', 'string.quoted.double.sbdsl');
  for (const text of ['循环', '次']) has(lines[5], text, 'keyword.control.sbdsl');
  has(lines[5], '移动并播报', 'support.function.command.sbdsl');
  has(lines[5], 'true', 'constant.language.boolean.sbdsl');
  has(lines[7], '当作为克隆体启动时', 'keyword.control.event.sbdsl');
});

test('namespace and explicit opcode calls have distinct scopes in every shape', () => {
  const lines = tokenize([
    '"自定义拓展".[移动 10 步]',
    '[说 ext::"read".($score)]',
    '如果 "自定义拓展"::"ready".<> 那么 { [说 "好"] }',
    "ext::\"when\".hat(/'space'/) { [等待 1 秒] }",
    '"自定义拓展".(x 坐标) 达标 { [说 "到达"] }'
  ].join('\n'));
  has(lines[0], '"自定义拓展"', 'entity.name.namespace.sbdsl');
  has(lines[0], '移动', 'support.function.command.sbdsl');
  has(lines[1], 'ext', 'entity.name.namespace.sbdsl');
  has(lines[1], 'read', 'entity.name.function.opcode.sbdsl');
  has(lines[1], '$score', 'variable.other.readwrite.sbdsl');
  has(lines[2], 'ready', 'entity.name.function.opcode.sbdsl');
  has(lines[3], 'hat', 'keyword.control.event.sbdsl');
  menu(lines[3], "/'space'/");
  has(lines[4], 'x 坐标', 'support.function.reporter.sbdsl');
  has(lines[4], '达标', 'keyword.control.event.sbdsl');
});

test('bare namespaces accept Unicode, hyphens and dots in every extension call shape', () => {
  const lines = tokenize([
    'scratch-tensor-extension.[执行计算]',
    '[说 自定义拓展.min.(结果数量)]',
    '如果 自定义拓展.<已就绪 ?> 那么 { [说 "好"] }',
    "自定义拓展-示例::\"when\".hat(/'space'/) { [等待 1 秒] }",
    '自定义拓展.min.当计算完成 { [说 "完成"] }',
    '[说 扩展１::"read".(1)]',
    'scratch-tensor-extension::"clear".[]',
    '如果 自定义拓展.min::"ready".<> 那么 { [说 "好"] }'
  ].join('\n'));
  for (const [index, name] of [[0, 'scratch-tensor-extension'], [1, '自定义拓展.min'], [2, '自定义拓展'],
    [3, '自定义拓展-示例'], [4, '自定义拓展.min'], [5, '扩展１'], [6, 'scratch-tensor-extension'], [7, '自定义拓展.min']]) {
    has(lines[index], name, 'entity.name.namespace.sbdsl');
    lacks(lines[index], name, 'string.');
  }
  has(lines[0], '执行计算', 'support.function.command.sbdsl');
  has(lines[1], '结果数量', 'support.function.reporter.sbdsl');
  has(lines[2], '已就绪', 'support.function.boolean.sbdsl');
  has(lines[3], 'when', 'entity.name.function.opcode.sbdsl');
  has(lines[3], 'hat', 'keyword.control.event.sbdsl');
  has(lines[4], '当计算完成', 'keyword.control.event.sbdsl');
  has(lines[5], 'read', 'entity.name.function.opcode.sbdsl');
  has(lines[6], 'clear', 'entity.name.function.opcode.sbdsl');
  has(lines[7], 'ready', 'entity.name.function.opcode.sbdsl');
  const [numbers] = tokenize('[说 (1.5-0.25)] [说 ($score-1)] [说 (2.5 / 0.5)]');
  for (const value of ['1.5', '0.25', '2.5', '0.5']) {
    has(numbers, value, 'constant.numeric.sbdsl');
    lacks(numbers, value, 'entity.name.namespace.');
  }
  has(numbers, '-', 'keyword.operator.sbdsl');
  has(numbers, '-', 'keyword.operator.sbdsl', 1);
  const [bareHat, quotedHat] = tokenize('ext.版本1.0 完成 { [说 "完成"] }\n"ext".版本1.0 完成 { [说 "完成"] }');
  has(bareHat, 'ext.版本1', 'entity.name.namespace.sbdsl');
  has(bareHat, '.', 'punctuation.separator.namespace.sbdsl', 1);
  has(quotedHat, '"ext"', 'entity.name.namespace.sbdsl');
  has(quotedHat, '版本1.0', 'keyword.control.event.sbdsl');
});

test('subtraction keeps adjacent right namespaces whole after numeric and reference operands', () => {
  for (const left of ['1', '1.5', '$score', '@参数']) {
    for (const name of ['ext', 'scratch-tensor-extension', '自定义拓展', '中文-名称.min']) {
      for (const suffix of ['.(结果值)', '::"number".()']) {
        const [line] = tokenize(`[说 (${left}-${name}${suffix})]`);
        has(line, '-', 'keyword.operator.sbdsl');
        has(line, name, 'entity.name.namespace.sbdsl');
        const start = line.line.indexOf(name);
        for (let position = start; position < start + name.length; position++) {
          assert(scopesAt(line, position).includes('entity.name.namespace.sbdsl'), line.line + ': ' + scopesAt(line, position));
          assert(!scopesAt(line, position).some(scope => scope.startsWith('keyword.operator.')), line.line);
        }
        if (suffix.startsWith('::')) has(line, 'number', 'entity.name.function.opcode.sbdsl');
        else has(line, '结果值', 'support.function.reporter.sbdsl');
      }
    }
  }
  const [standalone] = tokenize('scratch-tensor-extension.[执行计算]');
  for (let position = 0; position < 'scratch-tensor-extension'.length; position++) {
    assert(scopesAt(standalone, position).includes('entity.name.namespace.sbdsl'));
  }
});

test('keywords inside strings and menus do not become declarations or control flow', () => {
  const [line] = tokenize("[说 \"@var score 如果 定义自制积木 🐱 #\"] [选择 /'@sprite 当绿旗被点击 #'/]");
  for (const text of ['@var', '如果', '定义自制积木', '🐱']) {
    has(line, text, 'string.quoted.double.sbdsl');
    lacks(line, text, 'keyword.');
    lacks(line, text, 'variable.');
  }
  for (const text of ['@sprite', '当绿旗被点击']) {
    has(line, text, 'string.quoted.other.menu.sbdsl');
    lacks(line, text, 'keyword.');
  }
});

test('incomplete strings, menus and quoted references recover at later statements', () => {
  for (const unfinished of ['[说 "未结束', "[说 /'未结束", "[说 /'未结束", '[说 $"未结束', '[说 $list:"未结束', '[说 @"未结束', '[说 (连接 "a" 和', "[等待直到 <$hp < 1"]) {
    const lines = tokenize(unfinished + '\n[移动 10 步]\n@var next = 1');
    has(lines[1], '移动', 'support.function.command.sbdsl');
    lacks(lines[1], '移动', 'string.');
    lacks(lines[1], '移动', 'variable.');
    has(lines[2], 'next', 'variable.other.readwrite.sbdsl');
    lacks(lines[2], 'next', 'meta.block.command.');
  }
});

test('valid multiline blocks, list initializers and block comments preserve context', () => {
  const lines = tokenize([
    '[将 $score 设为',
    '  (连接 "🐱" 和',
    '    ($list:记录 的第 1 项))',
    ']',
    '@list logs = [',
    '  "a",',
    '  2',
    ']',
    '[说',
    '#',
    '@var not_a_declaration = 1',
    ']',
    '#',
    '  "完成"',
    ']',
    '[隐藏]'
  ].join('\n'));
  has(lines[1], '连接', 'support.function.reporter.sbdsl');
  has(lines[2], '$list:记录', 'variable.other.array.sbdsl');
  has(lines[5], 'a', 'string.quoted.double.sbdsl');
  has(lines[6], '2', 'constant.numeric.sbdsl');
  has(lines[10], '@var', 'comment.block.sbdsl');
  has(lines[11], ']', 'comment.block.sbdsl');
  has(lines[13], '完成', 'string.quoted.double.sbdsl');
  has(lines[15], '隐藏', 'support.function.command.sbdsl');
  lacks(lines[15], '隐藏', 'comment.');
});

test('init allows all hash comment forms without hiding later JSON or declarations', () => {
  const lines = tokenize([
    '@init {',
    '  # 普通注释',
    '  "x": # 行内 # 10,',
    '  #',
    '  "fake": [false]',
    '  #',
    '  "costumes": [{"name": "猫 🐱", "path": "./猫.svg"}],',
    '  "visible": true',
    '}',
    '@var score = 0'
  ].join('\n'));
  has(lines[1], '普通注释', 'comment.line.number-sign.sbdsl');
  has(lines[2], '行内', 'comment.block.sbdsl');
  has(lines[2], '10', 'constant.numeric.json');
  has(lines[4], 'fake', 'comment.block.sbdsl');
  has(lines[6], 'costumes', 'support.type.property-name.json');
  has(lines[6], '猫 🐱', 'string.quoted.double.json');
  has(lines[7], 'true', 'constant.language.json');
  has(lines[9], 'score', 'variable.other.readwrite.sbdsl');
  lacks(lines[9], 'score', 'meta.embedded.block.buildsb');
  const [inline] = tokenize('@init # 初始状态 # {"x": 10}');
  has(inline, '初始状态', 'comment.block.sbdsl');
  has(inline, 'x', 'support.type.property-name.json');
  has(inline, '10', 'constant.numeric.json');
});

test('division preserves quoted and list reference scopes and supports all operators', () => {
  const [line] = tokenize("($\"血 量\" / 2) ($list:记录 / 2) (@\"步 数\" / 2) (1 + 2) (2 - 1) (2 * 3) <1 > 0> <1 = 1>");
  has(line, '$"血 量"', 'variable.other.readwrite.sbdsl');
  has(line, '$list:记录', 'variable.other.array.sbdsl');
  has(line, '@"步 数"', 'variable.parameter.sbdsl');
  for (let index = 0; index < 3; index++) has(line, '/', 'keyword.operator.sbdsl', index);
  for (const operator of ['+', '-', '*', ">", '=']) has(line, operator, 'keyword.operator.sbdsl');
});

test('ordinary infix operators support user examples and compact nested expressions', () => {
  for (const [source, operator] of [
    ['(1 + 2)', '+'], ['(1 - 2)', '-'], ['(2 * 3)', '*'],
    ['(8 / 2)', '/'], ['(7 mod 3)', 'mod'], ['(7mod3)', 'mod'], ['(7mod(3+2))', 'mod'],
    ["<$hp < 1>", "<"], ["<$score > 100>", ">"], ['<$score = 100>', '='],
    ['(1+2)', '+'], ['(1-2)', '-'], ['(2*3)', '*'], ['(8/2)', '/'],
    ["<$hp<1>", "<"], ["<$score>100>", ">"], ['<$score=100>', '='],
    ['((8/2)/(6/3))', '/'], ["<(1+2)>0>", ">"], ["<@n > 1>", ">"]
  ]) {
    const [line] = tokenize(source);
    has(line, operator, 'keyword.operator.sbdsl', operator === '<' ? 1 : 0);
    has(line, source.endsWith(')') ? ')' : '>', source.endsWith(')')
      ? 'punctuation.definition.reporter.end.sbdsl'
      : 'punctuation.definition.boolean.end.sbdsl',
    source.endsWith('>') && operator === ">" ? 1 : 0);
  }
});

test('greater-than consumes one operator and preserves nested boolean and later slot boundaries', () => {
  const [line] = tokenize("[自定义 <1 > 0> 2] ext.<模板 <@ok> 1> [等待直到 <不成立 <$score>100>>]");
  has(line, ">", 'keyword.operator.sbdsl');
  for (const occurrence of [1, 2, 3, 5, 6]) has(line, '>', 'punctuation.definition.boolean.end.sbdsl', occurrence);
  has(line, ">", 'keyword.operator.sbdsl', 4);
  has(line, '@ok', 'variable.parameter.boolean.sbdsl');
  has(line, '2', 'constant.numeric.sbdsl');
  lacks(line, '2', 'meta.block.boolean.');
  const [missingOperator] = tokenize('<1>');
  has(missingOperator, '>', 'punctuation.definition.boolean.end.sbdsl');
  lacks(missingOperator, '>', 'keyword.operator.');
});

test('comparison operators and surrounding boolean delimiters have distinct scopes', () => {
  for (const source of ['<$hp < 1>', '<$score > 100>', '<$hp<1>', '<$score>100>',
    '<(8 / 2) > 1>', '<((1 + 2) * 3) < 10>', '<ext.(返回值) > 1>',
    '<"< >" > "<">', "</'a>b'/ < /'z'/>"]) {
    const [line] = tokenize('[测试 ' + source + ' 2]');
    const start = line.line.indexOf(source);
    const end = start + source.length - 1;
    assert(scopesAt(line, start).includes('punctuation.definition.boolean.begin.sbdsl'), source);
    assert(scopesAt(line, end).includes('punctuation.definition.boolean.end.sbdsl'), source);
    assert(!scopesAt(line, start).includes('keyword.operator.sbdsl'), source);
    assert(!scopesAt(line, end).includes('keyword.operator.sbdsl'), source);
    assert(line.tokens.some(token => token.scopes.includes('keyword.operator.sbdsl') &&
      /^[<>]$/.test(line.line.slice(token.startIndex, token.endIndex))), source);
    assert(line.tokens.some(token => token.scopes.includes('keyword.operator.expression.comparison.sbdsl') &&
      /^[<>]$/.test(line.line.slice(token.startIndex, token.endIndex))), source);
    const trailing = line.line.lastIndexOf('2');
    assert(!scopesAt(line, trailing).includes('meta.block.boolean.sbdsl'), source);
  }
  const [nested] = tokenize('[等待直到 <<$hp < 1> 或 <$score > 100>>]');
  has(nested, '或', 'keyword.operator.logical.sbdsl');
  for (const index of [0, 2, 3]) has(nested, '>', 'punctuation.definition.boolean.end.sbdsl', index);
});

test('comparison scopes allow visible theme contrast without recoloring arithmetic or menus', async () => {
  // This fixed palette checks TextMate scope fallback, not any installed theme.
  const registry = createRegistry({ settings: [
    { settings: { foreground: '#112233', background: '#FFFFFF' } },
    { scope: 'keyword.operator', settings: { foreground: '#112233' } },
    { scope: 'keyword.operator.expression', settings: { foreground: '#445566' } }
  ] });
  try {
    const themed = await registry.loadGrammar('source.sbdsl');
    const source = "<$hp < 1> <$score > 100> <1 = 1> (1 + 2) (8 / 2) /'<'/";
    const result = themed.tokenizeLine2(source, tm.INITIAL);
    const colors = registry.getColorMap();
    function colorAt(position) {
      for (let index = 0; index < result.tokens.length; index += 2) {
        const end = result.tokens[index + 2] ?? source.length;
        if (result.tokens[index] <= position && position < end) {
          return colors[(result.tokens[index + 1] >>> 15) & 511];
        }
      }
      assert.fail('No token color at ' + position);
    }
    for (const operator of [' < ', ' > ']) {
      assert.equal(colorAt(source.indexOf(operator) + 1), '#445566');
    }
    for (const fragment of ['<$hp', '1>', '<$score', '100>', ' = ', ' + ', ' / ', "/'<'/"]) {
      const position = source.indexOf(fragment) + (fragment.endsWith('>') ? fragment.length - 1 : fragment.startsWith(' ') ? 1 : 0);
      assert.equal(colorAt(position), '#112233', fragment);
    }
    const [tokens] = tokenize(source, themed);
    for (const text of ['=', '+', '/']) {
      lacks(tokens, text, 'keyword.operator.expression.comparison.');
    }
    lacks(tokens, '<', 'keyword.operator.expression.comparison.', 4);
  } finally {
    registry.dispose();
  }
});

test('infix whitespace and comments can span lines without losing closing delimiters', () => {
  const lines = tokenize([
    '[说 (8',
    ' / (6 / 2))]',
    '如果 <$score',
    ' # threshold #',
    " >",
    ' 100',
    '> 那么 { [说 "完成"] }',
    '[说 (',
    ' -1.5e-2',
    ' / -2e+3',
    ')]'
  ].join('\n'));
  has(lines[1], '/', 'keyword.operator.sbdsl');
  has(lines[4], ">", 'keyword.operator.sbdsl');
  has(lines[4], ">", 'keyword.operator.expression.comparison.sbdsl');
  has(lines[6], '>', 'punctuation.definition.boolean.end.sbdsl');
  has(lines[6], '那么', 'keyword.control.sbdsl');
  has(lines[8], '-1.5e-2', 'constant.numeric.sbdsl');
  has(lines[9], '/', 'keyword.operator.sbdsl');
  has(lines[9], '-2e+3', 'constant.numeric.sbdsl');
});

test('negative numbers and exponents stay numeric while subtraction remains an operator', () => {
  for (const [source, operator, numeric] of [
    ['(-1 + -2)', '+', '-2'], ['(1--2)', '-', '-2'],
    ['(1 -2)', '-', '2'], ['(-1.5e-2/-2e+3)', '/', '-2e+3'],
    ['(1e-3+2e+4)', '+', '2e+4']
  ]) {
    const [line] = tokenize(source);
    has(line, operator, 'keyword.operator.sbdsl');
    has(line, numeric, 'constant.numeric.sbdsl');
  }
});

test('division and menus remain distinct in independent slots and either operand', () => {
  const [line] = tokenize("[自定义 8 /'*'/ /'#'/] [说 (/'*'/ / /'#'/)] [说 (8 / /'#'/)] [说 (8//''/)]");
  menu(line, "/'*'/");
  menu(line, "/'#'/");
  menu(line, "/'*'/", 1);
  menu(line, "/'#'/", 1);
  menu(line, "/'#'/", 2);
  lacks(line, '#', 'comment.');
  const [slots] = tokenize("(自定义 8 /'*'/ /'#'/) ext.<模板 <@ok> /'#'/>");
  menu(slots, "/'*'/");
  menu(slots, "/'#'/");
  const [compact] = tokenize("(8//''/)");
  has(compact, '/', 'keyword.operator.sbdsl');
  menu(compact, "/''/");
  const multiline = tokenize("(\n /'#'/\n # between operands #\n /\n /'*'/\n)\n[移动 1 步]");
  menu(multiline[1], "/'#'/");
  has(multiline[2], 'between operands', 'comment.block.sbdsl');
  has(multiline[3], '/', 'keyword.operator.sbdsl');
  menu(multiline[4], "/'*'/");
  has(multiline[6], '移动', 'support.function.command.sbdsl');
  lacks(multiline[6], '移动', 'meta.block.reporter.');
  for (const source of ["(8 /'*'/)", "ext.(8 /'*'/)", "(8 /'#'/)", "ext.(8 /'a/b'/)"]) {
    const [menuSlot, following] = tokenize(source + '\n[移动 1 步]');
    menu(menuSlot, source.slice(source.indexOf('/'), source.lastIndexOf('/') + 1));
    lacks(menuSlot, '/', 'invalid.illegal.operator.');
    has(menuSlot, ')', 'punctuation.definition.reporter.end.sbdsl');
    has(following, '移动', 'support.function.command.sbdsl');
    lacks(following, '移动', 'meta.block.reporter.');
  }
});

test('compact mod does not split variable or parameter identifiers containing mod', () => {
  const [line] = tokenize('(7mod3) ($mod3+1) ($amodb/2) ($global:model*2) (@modCount-1)');
  has(line, 'mod', 'keyword.operator.sbdsl');
  has(line, '3', 'constant.numeric.sbdsl');
  for (const name of ['$mod3', '$amodb', '$global:model']) {
    has(line, name, 'variable.other.readwrite.sbdsl');
    lacks(line, name, 'keyword.operator.');
  }
  has(line, '@modCount', 'variable.parameter.sbdsl');
});

test('retired starred operators are invalid while identical menu and string text is preserved', () => {
  for (const operator of ['+*', '-*', '**', '/*', '=*', '<*', '>*']) {
    const comparison = ['=*', '<*', '>*'].includes(operator);
    const [line] = tokenize((comparison ? '<1 ' : '(1 ') + operator + ' 2' +
      (comparison ? '>' : ')'));
    has(line, operator, 'invalid.illegal.operator.sbdsl');
    lacks(line, operator, 'keyword.operator.');
  }
  const [literal] = tokenize("[说 \"+* -* ** /* > =*\"] [说 /'*'/] [说 /'>'/]");
  has(literal, '+*', 'string.quoted.double.sbdsl');
  menu(literal, "/'*'/");
  has(literal, ">", 'string.quoted.double.sbdsl');
});

test('build files support bare root fields, nested arrays and all hash comments', () => {
  const lines = tokenize([
    '# 构建配置',
    'entry: "./src/main.sbdsl"',
    'output: # 当前配置 # "./dist/工程.sb3"',
    'extensions: [',
    '  "./extensions/一.js"',
    '  "./extensions/二.js",',
    ']',
    '#',
    'ignored: "多行注释"',
    '结束 # stage: {',
    '  backdrops: [{name: "猫\\u732b\\t\\/#", path: "./猫.svg",}]',
    '}',
    '"runtime": {fps: 60, fencing: false,}',
  ].join('\n'), buildGrammar);
  for (const [index, key] of [[1, 'entry'], [2, 'output'], [3, 'extensions'], [9, 'stage'], [10, 'backdrops'], [12, 'runtime']]) {
    has(lines[index], key, 'support.type.property-name.json');
  }
  has(lines[0], '构建配置', 'comment.line.number-sign.sbdsl');
  has(lines[2], '当前配置', 'comment.block.sbdsl');
  has(lines[4], './extensions/一.js', 'string.quoted.double.json');
  has(lines[8], 'ignored', 'comment.block.sbdsl');
  has(lines[10], '\\u732b', 'constant.character.escape.json');
  has(lines[10], '\\t', 'constant.character.escape.json');
  lacks(lines[10], '#', 'comment.');
  has(lines[12], 'false', 'constant.language.json');
  const [braced] = tokenize('{entry: "main.sbdsl", "output": "project.sb3",}', buildGrammar);
  has(braced, 'entry', 'support.type.property-name.json');
  has(braced, 'output', 'support.type.property-name.json');
  const [invalid] = tokenize('中文: "quoted values"', buildGrammar);
  has(invalid, '中文', 'invalid.illegal.json');
});

test('root extension aliases distinguish paths, the as keyword and namespace strings', () => {
  for (const wrapper of [false, true]) {
    const source = [
      'entry: "main.sbdsl"',
      'extensions: [',
      '  "./extensions/自定义拓展.js" as "newname"',
      '  "./extensions/其他.js"',
      '  # before as # as # after as # "新-名称.min"',
      '  "./extensions/第三.js" # before newline',
      '  as',
      '  #',
      '  ignored as "not-an-alias"',
      '  #',
      '  "转义\\u540d称"',
      '  "./extensions/原名.js",',
      ']',
      'output: "project.sb3"'
    ].join('\n');
    const lines = tokenize(wrapper ? '{\n' + source + '\n}' : source, buildGrammar).slice(wrapper ? 1 : 0);
    has(lines[1], 'extensions', 'support.type.property-name.json');
    for (const index of [2, 3, 5, 11]) has(lines[index], './extensions/', 'string.quoted.double.json');
    for (const index of [2, 4, 6]) has(lines[index], 'as', 'keyword.control.import.sbdsl', index === 4 ? 1 : 0);
    for (const [index, text] of [[2, 'newname'], [4, '新-名称.min'], [10, '转义']]) {
      has(lines[index], text, 'entity.name.namespace.sbdsl');
      lacks(lines[index], text, 'string.');
    }
    has(lines[10], '\\u540d', 'constant.character.escape.json');
    has(lines[8], 'ignored', 'comment.block.sbdsl');
    has(lines[13], 'output', 'support.type.property-name.json');
    lacks(lines[13], 'project.sb3', 'entity.name.namespace.');
  }
  const [quoted] = tokenize('"extensions": ["a.js"# first #as# second #"alias", "b.js"]', buildGrammar);
  has(quoted, 'as', 'keyword.control.import.sbdsl');
  has(quoted, 'alias', 'entity.name.namespace.sbdsl');
  has(quoted, 'b.js', 'string.quoted.double.json');
});

test('extension alias keywords stay out of ordinary fields, nested data and init', () => {
  const lines = tokenize([
    'name: "as"',
    'entry: "main.sbdsl" as "wrong"',
    'runtime: { extensions: ["a.js" as "nested"] }',
    'stage:',
    '{ extensions: ["b.js" as "nested-again"] }',
    'extensions: ["./as.js" as "alias"]',
    'other: ["c.js" as "wrong-again"]'
  ].join('\n'), buildGrammar);
  has(lines[0], 'as', 'string.quoted.double.json');
  for (const index of [1, 2, 4, 6]) {
    lacks(lines[index], 'as', 'keyword.control.import.');
    lacks(lines[index], index === 1 ? 'wrong' : index === 2 ? 'nested' : index === 4 ? 'nested-again' : 'wrong-again', 'entity.name.namespace.');
  }
  has(lines[5], 'as', 'keyword.control.import.sbdsl', 1);
  has(lines[5], 'alias', 'entity.name.namespace.sbdsl');
  const init = tokenize('@init {\n  extensions: ["a.js" as "wrong"]\n}\n[移动 1 步]');
  lacks(init[1], 'as', 'keyword.control.import.');
  lacks(init[1], 'wrong', 'entity.name.namespace.');
  has(init[3], '移动', 'support.function.command.sbdsl');
});

test('standalone build files and init use the same data grammar', () => {
  const fields = 'x: 10\nvisible: false\ncostumes: [{name: "猫", path: "cat.svg"}]';
  const config = tokenize(fields, buildGrammar);
  const init = tokenize('@init {\n' + fields + '\n}');
  for (const [line, value] of [[0, 'x'], [0, '10'], [1, 'visible'], [1, 'false'], [2, 'name'], [2, '猫']]) {
    assert.equal(scopes(config[line], value).at(-1), scopes(init[line + 1], value).at(-1));
  }
});

test('build language activates alone and has a title-bar build action', () => {
  const manifest = require('../package.json');
  const language = manifest.contributes.languages.find(item => item.id === 'sbdsl-build');
  assert.deepEqual(language.filenames, ['sbdsl.buildsb']);
  assert.deepEqual(language.extensions, ['.buildsb']);
  const source = manifest.contributes.languages.find(item => item.id === 'sbdsl');
  for (const theme of ['light', 'dark']) {
    assert.notEqual(language.icon[theme], source.icon[theme]);
    assert.ok(fs.existsSync(path.join(__dirname, '..', language.icon[theme])));
  }
  assert.ok(manifest.activationEvents.includes('onLanguage:sbdsl-build'));
  const action = manifest.contributes.menus['editor/title'].find(item => item.command === 'scratchdsl.build');
  assert.equal(action.group, 'navigation@1');
  assert.ok(action.when.includes('resourceExtname == .buildsb'));
  assert.ok(manifest.contributes.commands.find(item => item.command === action.command).icon);
});
