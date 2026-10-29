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
  assert(text.startsWith("'") && text.endsWith("'"), 'complete menu: ' + text);
  for (const [position, boundary] of [[start, 'begin'], [start + text.length - 1, 'end']]) {
    const scopes = scopesAt(row, position);
    assert(scopes.includes('meta.menu.sbdsl'), text + ': ' + scopes);
    assert(scopes.includes(`punctuation.definition.menu.${boundary}.sbdsl`), text + ': ' + scopes);
    assert(!scopes.some(scope => scope.startsWith('string.')), 'menu quote inherits content scope: ' + scopes);
  }
  for (let position = start + 1; position < start + text.length - 1; position++) {
    const scopes = scopesAt(row, position);
    assert(scopes.includes('string.quoted.single.menu.sbdsl'), text + ': ' + scopes);
    assert(!scopes.some(scope => scope.startsWith('punctuation.definition.menu.')), text + ': ' + scopes);
  }
}
function has(row, text, scope, occurrence = 0) {
  const actual = scopes(row, text, occurrence);
  assert(actual.includes(scope), text + ': ' + actual);
}
function lacks(row, text, prefix, occurrence = 0) {
  const actual = scopes(row, text, occurrence);
  assert(!actual.some(scope => scope.startsWith(prefix)), text + ': ' + actual);
}

test('hash comments distinguish line, inline and multiline blocks', () => {
  const rows = tokenize('# 行注释\n说(1) # 中间 #\n  #\n多行\n结束 # 说(3)\n说("#字符串")');
  has(rows[0], '行注释', 'comment.line.number-sign.sbdsl');
  has(rows[1], '中间', 'comment.block.sbdsl');
  has(rows[3], '多行', 'comment.block.sbdsl');
  lacks(rows[4], '3', 'comment.');
  has(rows[5], '#字符串', 'string.quoted.double.sbdsl');
});

test('menus retain hashes, stars, empty values and quoted slash content', () => {
  for (const value of ["'选项'", "''", "'*'", "'#'", "'a/b\"#'"]) {
    const [row] = tokenize('"自定义拓展".模式' + value);
    menu(row, value);
    has(row, '"自定义拓展"', 'entity.name.namespace.sbdsl');
  }
});

test('menu delimiters remain punctuation in scalar inputs and static menu fields', () => {
  for (const value of ["'梯度'", "''", "'*'", "'/'", "'a/b'", "'a\"/#'"]) {
    const [row] = tokenize('说("scratch-tensor-extension".获取 Tensor("W")的' + value + ')');
    menu(row, value);
    has(row, ')', 'punctuation.definition.reporter.end.sbdsl');
  }
});

test('menus require single quotes and use JSON escapes plus escaped apostrophes', () => {
  const value = String.raw`'a\'b\n\r\t\b\f\u732b\/\\\"#()<>/"'`;
  const [row] = tokenize('模式' + value);
  menu(row, value);
  for (const escape of [String.raw`\'`, String.raw`\n`, String.raw`\r`, String.raw`\t`,
    String.raw`\b`, String.raw`\f`, String.raw`\u732b`, String.raw`\/`, String.raw`\\`, String.raw`\"`]) {
    has(row, escape, 'constant.character.escape.sbdsl');
  }
  lacks(row, '#', 'comment.');
  for (const legacy of ['/space/', '//', '/*/', '/"space"/']) {
    const [row] = tokenize('模式' + legacy);
    assert(!row.tokens.some(token => token.scopes.includes('meta.menu.sbdsl')));
  }
  const [invalid] = tokenize(String.raw`模式'bad\q'`);
  has(invalid, String.raw`\q`, 'invalid.illegal.escape.sbdsl');
});

test('logical words in command and reporter template text retain their text scopes', () => {
  const [command] = tokenize('ext.张量("A")and("B")进行\'*\'运算赋值给("product")');
  for (const text of ['张量', 'and', '进行', '运算赋值给']) {
    has(command, text, 'support.function.command.sbdsl');
    lacks(command, text, 'keyword.operator.logical.');
  }
  const [reporter] = tokenize('说(连接("A")and("B")or("C")not)');
  for (const text of ['连接', 'and', 'or', 'not']) {
    has(reporter, text, 'support.function.reporter.sbdsl');
    lacks(reporter, text, 'keyword.operator.logical.');
  }
});

test('logical operators are highlighted within boolean inputs and isolated from literals', () => {
  const [row] = tokenize('等待直到<<true>and<not<false>>>');
  for (const text of ['and', 'not']) {
    has(row, text, 'keyword.operator.logical.sbdsl');
    lacks(row, text, 'support.function.command.');
  }
  const [literal] = tokenize('说("and or not") 模式\'and or not\'');
  for (const text of ['and', 'or', 'not']) {
    has(literal, text, 'string.quoted.double.sbdsl');
    has(literal, text, 'string.quoted.single.menu.sbdsl', 1);
  }
});

test('division and comparison operators do not start menus or nested boolean inputs', () => {
  const [row] = tokenize('说(8 / (6 / 2)) <hp < 1> (hp / 2) (("x") / 2)');
  for (let i = 0; i < 4; i++) has(row, '/', 'keyword.operator.sbdsl', i);
  has(row, '<', 'keyword.operator.sbdsl', 1);
  const [menuRow] = tokenize('选择(8)\'* abc\'');
  menu(menuRow, "'* abc'");
});

test('init shares build syntax with bare keys and JSON-only escapes', () => {
  const rows = tokenize('init:\n  costumes: [{name:"\\u732b\\t\\/#", path:"../assets/a.svg"}]\n  visible: false\n\n说("\\t")');
  has(rows[0], 'init', 'keyword.control.sbdsl');
  for (const text of ['costumes', 'name']) has(rows[1], text, 'support.type.property-name.json');
  for (const text of ['\\u732b', '\\t']) has(rows[1], text, 'constant.character.escape.json');
  lacks(rows[1], '#', 'comment.');
  has(rows[2], 'false', 'constant.language.json');
  has(rows[4], '\\t', 'invalid.illegal.escape.sbdsl');
  lacks(rows[4], '说', 'meta.embedded.block.buildsb');
});

test('bare calls use no shape brackets while data configuration keeps array pairing', () => {
  const config = require('../language-configuration.json');
  assert.equal(config.comments, undefined);
  assert(!config.autoClosingPairs.some(pair => ['/', '<'].includes(pair.open)));
  for (const open of ['(', '[', '{', "'", '"']) assert(config.autoClosingPairs.some(pair => pair.open === open));
  const [legacy] = tokenize('[移动 10 步]');
  for (const delimiter of ['[', ']']) has(legacy, delimiter, 'invalid.illegal.delimiter.sbdsl');
  const [modern] = tokenize('移动(10)步');
  has(modern, '移动', 'support.function.command.sbdsl');
  has(modern, '10', 'constant.numeric.sbdsl');
  has(modern, '步', 'support.function.command.sbdsl');
});

test('sleep highlights its seconds input as an ordinary scalar command', () => {
  const rows = tokenize('start:\n    sleep(1)\n    sleep(seconds)');
  for (const row of rows.slice(1)) has(row, 'sleep', 'support.function.command.sbdsl');
  has(rows[1], '1', 'constant.numeric.sbdsl');
  has(rows[2], 'seconds', 'variable.other.readwrite.sbdsl');
});

test('Python suites and data arrays keep matching indentation rules', () => {
  const config = require('../language-configuration.json');
  const indent = new RegExp(config.indentationRules.increaseIndentPattern);
  for (const text of ['start:', 'if <true>:', 'elif <false>:', 'else:', 'loop 10:', 'while true:',
    'def 测试 @n: # comment', 'ext.当<true>:', 'items = {', 'runtime: {', 'costumes: [']) {
    assert(indent.test(text), text);
  }
  assert(!indent.test('说("text:")'));
  const dedent = new RegExp(config.indentationRules.decreaseIndentPattern);
  for (const text of ['    elif <true>:', '    else:', '    }', '    ]']) assert(dedent.test(text), text);
  assert(!dedent.test('    end'));
});

test('end is no longer a structural keyword and literals retain their scopes', () => {
  const rows = tokenize('start:\n    if <true>:\n        pass\n    end\nend\nend = 1\n说("end")');
  for (const i of [3, 4, 5]) lacks(rows[i], 'end', 'keyword.control.');
  has(rows[5], 'end', 'variable.other.readwrite.sbdsl');
  has(rows[6], 'end', 'string.quoted.double.sbdsl');
});

test('extension C headers preserve namespaces, typed inputs and repeated branches', () => {
  const rows = tokenize('start:\n    ext.当<ready = 1>否则(2):\n        score += 1\n    else:\n        score -= 1\n    else:\n        pass\n    "我的 扩展"::"repeat"((count+1)):\n        说(score)\n    ext.普通命令("text:")');
  for (const i of [1, 7]) {
    has(rows[i], i === 1 ? 'ext' : '"我的 扩展"', 'meta.control.extension.sbdsl');
    has(rows[i], ':', i === 1 ? 'punctuation.section.block.begin.sbdsl' : 'punctuation.separator.namespace.sbdsl');
  }
  has(rows[1], 'ext', 'entity.name.namespace.sbdsl');
  has(rows[1], '否则', 'support.function.command.sbdsl');
  has(rows[1], 'ready', 'variable.other.readwrite.sbdsl');
  for (const i of [3, 5]) has(rows[i], 'else', 'keyword.control.sbdsl');
  has(rows[2], '+=', 'keyword.operator.assignment.sbdsl');
  has(rows[7], 'repeat', 'entity.name.function.opcode.sbdsl');
  has(rows[8], '说', 'support.function.command.sbdsl');
  lacks(rows[9], 'ext', 'meta.control.extension.');
});

test('scalar inputs wrap literals, variable values, reporters and nested arithmetic', () => {
  for (const source of ['说(a+b-c*d/f%e)', '说((a+b)*(c-d)/f%e)']) {
    const [row] = tokenize(source);
    for (const text of ['a', 'b', 'c', 'd', 'f', 'e']) has(row, text, 'variable.other.readwrite.sbdsl');
    for (const text of ['+', '-', '*', '/', '%']) has(row, text, 'keyword.operator.sbdsl');
  }
  const [row] = tokenize('说(连接("你好")和("世界"))');
  has(row, '说', 'support.function.command.sbdsl');
  has(row, '连接', 'support.function.reporter.sbdsl');
  for (const text of ['你好', '世界']) has(row, text, 'string.quoted.double.sbdsl');
});

test('assignment sugar highlights targets, six operators and unwrapped math', () => {
  for (const [target, op] of [['score', '='], ['分数', '+='], ['global:score', '-='], ['血量', '*='], ['global:总分', '/='], ['score', '%=']]) {
    const [row] = tokenize(`    ${target} ${op} a+b-c*d/f%e`);
    has(row, target, 'variable.other.readwrite.sbdsl');
    has(row, op, 'keyword.operator.assignment.sbdsl');
    for (const text of ['a', 'b', 'c', 'd', 'f', 'e']) assert(scopesAt(row, row.line.lastIndexOf(text)).includes('variable.other.readwrite.sbdsl'));
    for (const text of ['+', '-', '*', '/', '%']) assert(scopesAt(row, row.line.lastIndexOf(text)).includes('keyword.operator.sbdsl'));
  }
  const [row] = tokenize('# before # score # target # += bonus # after');
  has(row, 'score', 'variable.other.readwrite.sbdsl');
  has(row, '+=', 'keyword.operator.assignment.sbdsl');
  has(row, 'target', 'comment.block.sbdsl');
  has(row, 'after', 'comment.line.number-sign.sbdsl');
});

test('assignment values preserve grouped reporters, booleans, literal text and statement boundaries', () => {
  const rows = tokenize('score = (连接("a")("b"))\nflag = <score>0>\ntext = "# $score [ ]"\nscore = (\n    8 / (6 / 2)\n)\n移动(1)步');
  has(rows[0], '连接', 'support.function.reporter.sbdsl');
  has(rows[1], '>', 'keyword.operator.expression.comparison.sbdsl');
  has(rows[2], '$score', 'string.quoted.double.sbdsl');
  has(rows[4], '/', 'keyword.operator.sbdsl');
  has(rows[6], '移动', 'support.function.command.sbdsl');
  lacks(rows[6], '移动', 'meta.assignment.');
});

test('declarations and non-scalar references do not become scalar assignments', () => {
  for (const text of ['var score = 0', 'list logs = {}', '@param = 1', '(logs) = 1']) {
    const [row] = tokenize(text);
    lacks(row, '=', 'meta.assignment.');
  }
  const [decl] = tokenize('global:score = 0');
  has(decl, 'global:score', 'variable.other.readwrite.sbdsl');
});

test('ordinary identifiers stay bare inside scalar inputs and sugar contexts', () => {
  const rows = tokenize('移动(speed)步\n说(global:score)\nloop count:\n    if <score > 1>:\n        说(score)');
  for (const [i, text] of [[0, 'speed'], [1, 'global:score'], [2, 'count'], [3, 'score'], [4, 'score']]) has(rows[i], text, 'variable.other.readwrite.sbdsl');
  for (const text of ['移动', '步']) has(rows[0], text, 'support.function.command.sbdsl');
});

test('variable and list identities use typed parenthesized slots', () => {
  const rows = tokenize('显示变量(score)\n将(score)加入(logs)\n删除(global:logs)的第(1)项\n说((logs)的项目数)');
  has(rows[0], 'score', 'variable.other.readwrite.sbdsl');
  has(rows[1], 'score', 'variable.other.readwrite.sbdsl');
  has(rows[1], 'logs', 'variable.other.array.sbdsl');
  has(rows[2], 'global:logs', 'variable.other.array.sbdsl');
  has(rows[3], 'logs', 'variable.other.array.sbdsl');
});

test('retired data prefixes and quoted data names remain invalid', () => {
  for (const ref of ['var:score', 'list:logs', 'global:list:logs', 'global:"score"']) {
    const [row] = tokenize('说(' + ref + ')');
    has(row, ref, 'invalid.illegal.reference.sbdsl');
  }
  for (const declaration of ['"score" = 0', '"logs" = {}']) {
    const [row] = tokenize(declaration);
    has(row, declaration.includes('score') ? '"score"' : '"logs"', 'invalid.illegal.identifier.sbdsl');
  }
  const [labels] = tokenize('ext.处理 list: (1) var: (2) global: (3)');
  for (const text of ['list:', 'var:', 'global:']) lacks(labels, text, 'invalid.');
});

test('registered reporters differ from variable values and hyphenated namespaces', () => {
  const rows = tokenize('说(计时器)\nscore = 计时器\nscore = score - ext.数字\n说(ext-name.数字)');
  has(rows[0], '计时器', 'support.function.reporter.sbdsl');
  has(rows[3], 'ext-name', 'entity.name.namespace.sbdsl');
  has(rows[2], '-', 'keyword.operator.sbdsl');
});

test('retired dollar references do not affect string or menu contents', () => {
  for (const ref of ['$score', '$list:logs', '$global:score', '$"a b"']) {
    const [row] = tokenize('说(' + ref + ')');
    has(row, ref, 'invalid.illegal.reference.sbdsl');
  }
  const [row] = tokenize('说("$score") 模式\'$list:logs\'');
  has(row, '$score', 'string.quoted.double.sbdsl');
  has(row, '$list:logs', 'string.quoted.single.menu.sbdsl');
});

test('curly list literals and JSON init arrays keep distinct scopes', () => {
  const rows = tokenize('logs = {1, "a"}\nsprite "玩家":\n    init:\n        costumes: [{name: "猫", path: "a.svg"}]\n    start:\n        将("b")加入(logs)');
  has(rows[0], '{', 'punctuation.definition.list.begin.sbdsl');
  has(rows[3], 'costumes', 'support.type.property-name.json');
  has(rows[4], 'start', 'keyword.control.event.sbdsl');
  has(rows[5], '加入', 'support.function.command.sbdsl');
  lacks(rows[5], '加入', 'meta.embedded.');
});

test('all command text remains colored around typed inputs', () => {
  const [row] = tokenize('在(2)秒内滑行到 x:(10)y:(20)');
  for (const text of ['在', '秒内滑行到', 'x:', 'y:']) has(row, text, 'support.function.command.sbdsl');
  for (const text of ['2', '10', '20']) has(row, text, 'constant.numeric.sbdsl');
});

test('declarations identify ordinary data names, quoted roles and modules', () => {
  const rows = tokenize('分数 = 1\n日志 = {}\nsprite "玩家 😀":\n    init:\n        pass\nimport "目录.模块"');
  has(rows[0], '分数', 'variable.other.readwrite.sbdsl');
  has(rows[1], '日志', 'variable.other.array.sbdsl');
  has(rows[2], '玩家', 'entity.name.type.sprite.sbdsl');
  has(rows[5], '目录.模块', 'entity.name.namespace.module.sbdsl');
});

test('ordinary and quoted procedure parameter references keep their types', () => {
  const [row] = tokenize('处理(@steps)(@"步 数")<@ready><@"准备 好">');
  for (const text of ['@steps', '@"步 数"']) has(row, text, 'variable.parameter.sbdsl');
  for (const text of ['@ready', '@"准备 好"']) has(row, text, 'variable.parameter.boolean.sbdsl');
});

test('procedure templates, events and control flow preserve fixed text', () => {
  const rows = tokenize('def 前进 @n 判断 <@ok> @warp:\n    移动(@n)步\n当按下\'space\'键:\n    if <@ok>:\n        前进(2)<true>\n    elif <false>:\n        pass\n    else:\n        while true:\n            sleep(1)');
  has(rows[0], '前进', 'entity.name.function.sbdsl');
  has(rows[0], '@warp', 'storage.modifier.sbdsl');
  has(rows[2], '当按下', 'keyword.control.event.sbdsl');
  for (const i of [3, 5, 7, 8]) has(rows[i], ['if', 'elif', 'else', 'while'][[3,5,7,8].indexOf(i)], 'keyword.control.sbdsl');
});

test('normal and explicit extension calls place namespaces inside value inputs', () => {
  const rows = tokenize('ext.移动(10)步\n说(ext.数字)\n等待直到<ext.就绪>\next::"move"((10))\n说(ext::"number"())\n等待直到<ext::"ready"()>');
  for (const row of rows) has(row, 'ext', 'entity.name.namespace.sbdsl');
  for (const [i, op] of [[3, 'move'], [4, 'number'], [5, 'ready']]) has(rows[i], op, 'entity.name.function.opcode.sbdsl');
  for (const old of ['ext.[移动 10 步]', 'ext::"x".[]', 'ext::"x".hat()']) {
    const [row] = tokenize(old);
    assert(row.tokens.some(t => t.scopes.some(s => s.startsWith('invalid.illegal.'))), old);
  }
});

test('extension namespaces accept Unicode, hyphens, dots and quoted names', () => {
  for (const ns of ['自定义拓展', 'custom.min', 'tensor-tools', '"我的 扩展 😀"']) {
    for (const call of [ns + '.移动(1)步', '说(' + ns + '.值)', '<' + ns + '.就绪>', ns + '::"move"((1))']) {
      const [row] = tokenize(call);
      has(row, ns, 'entity.name.namespace.sbdsl');
    }
  }
});

test('subtraction preserves right-hand namespaces and grouped qualified inputs', () => {
  for (const source of ['score = 8-ext.数字', 'score = score - ext.数字', '说(8-ext.数字)', '说(@n-ext.数字)']) {
    const [row] = tokenize(source);
    has(row, 'ext', 'entity.name.namespace.sbdsl');
    has(row, '-', 'keyword.operator.sbdsl');
  }
});

test('keywords inside strings and menus never become declarations or flow', () => {
  const [row] = tokenize('说("var list if loop start") 模式\'var list if loop start\'');
  for (const text of ['var', 'list', 'if', 'loop', 'start']) {
    has(row, text, 'string.quoted.double.sbdsl');
    has(row, text, 'string.quoted.single.menu.sbdsl', 1);
  }
});

test('incomplete quotes recover for subsequent bare statements', () => {
  for (const line of ['说("unfinished', '模式\'unfinished', '说(@"unfinished']) {
    const rows = tokenize(line + '\n移动(1)步');
    lacks(rows[1], '移动', 'string.');
    lacks(rows[1], '移动', 'variable.parameter.');
  }
});

test('multiline typed inputs, list literals and block comments preserve contexts', () => {
  const rows = tokenize('说(连接(\n    "a"\n)和(\n    "b"\n))\n日志 = {\n    1,\n    "二"\n}\n#\n移动(0)步\n#\n移动(1)步');
  has(rows[0], '连接', 'support.function.reporter.sbdsl');
  has(rows[1], 'a', 'string.quoted.double.sbdsl');
  has(rows[3], 'b', 'string.quoted.double.sbdsl');
  has(rows[6], '1', 'constant.numeric.sbdsl');
  has(rows[10], '移动', 'comment.block.sbdsl');
  has(rows[12], '移动', 'support.function.command.sbdsl');
});

test('init allows all hash comment forms without hiding later JSON or declarations', () => {
  const lines = tokenize([
    "init:",
    '  # 普通注释',
    '  "x": # 行内 # 10,',
    '  #',
    '  "fake": [false]',
    '  #',
    '  "costumes": [{"name": "猫 🐱", "path": "./猫.svg"}],',
    '  "visible": true',
    '',
    "score = 0"
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
  const [header, inline] = tokenize("init: # 初始状态 #\n    \"x\": 10");
  has(header, '初始状态', 'comment.block.sbdsl');
  has(inline, 'x', 'support.type.property-name.json');
  has(inline, '10', 'constant.numeric.json');
});

test('division preserves ordinary data and quoted parameter scopes and supports all operators', () => {
  const [line] = tokenize("(血量 / 2) (global:总分 / 2) (@\"步 数\" / 2) (1 + 2) (2 - 1) (2 * 3) <1 > 0> <1 = 1>");
  has(line, '血量', 'variable.other.readwrite.sbdsl');
  has(line, 'global:总分', 'variable.other.readwrite.sbdsl');
  has(line, '@"步 数"', 'variable.parameter.sbdsl');
  for (let index = 0; index < 3; index++) has(line, '/', 'keyword.operator.sbdsl', index);
  for (const operator of ['+', '-', '*', ">", '=']) has(line, operator, 'keyword.operator.sbdsl');
});

test('ordinary infix operators support user examples and compact nested expressions', () => {
  for (const [source, operator] of [
    ['(1 + 2)', '+'], ['(1 - 2)', '-'], ['(2 * 3)', '*'],
    ['(8 / 2)', '/'], ["(7 % 3)", "%"], ['(7%3)', "%"], ['(7%(3+2))', "%"],
    ["<hp < 1>", "<"], ["<score > 100>", ">"], ['<score = 100>', '='],
    ['(1+2)', '+'], ['(1-2)', '-'], ['(2*3)', '*'], ['(8/2)', '/'],
    ["<hp<1>", "<"], ["<score>100>", ">"], ['<score=100>', '='],
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

test('comparison operators and surrounding boolean delimiters have distinct scopes', () => {
  for (const source of ['<hp < 1>', '<score > 100>', '<hp<1>', '<score>100>',
    '<(8 / 2) > 1>', '<((1 + 2) * 3) < 10>', '<(ext.返回值) > 1>',
    '<"< >" > "<">', "<'a>b' < 'z'>"]) {
    const [line] = tokenize('测试' + source + '(2)');
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
  const [nested] = tokenize('等待直到<<hp < 1> or <score > 100>>');
  has(nested, 'or', 'keyword.operator.logical.sbdsl');
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
    const source = "<hp < 1> <score > 100> <1 = 1> (1 + 2) (8 / 2) '<'";
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
    for (const fragment of ['<hp', '1>', '<score', '100>', ' = ', ' + ', ' / ', "'<'"]) {
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
    '说(8',
    ' / (6 / 2))',
    "if <score",
    ' # threshold #',
    " >",
    ' 100',
    '>:',
    '说(',
    ' -1.5e-2',
    ' / -2e+3',
    ')'
  ].join('\n'));
  has(lines[1], '/', 'keyword.operator.sbdsl');
  has(lines[4], ">", 'keyword.operator.sbdsl');
  has(lines[4], ">", 'keyword.operator.expression.comparison.sbdsl');
  has(lines[6], '>', 'punctuation.definition.boolean.end.sbdsl');
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
  const [line] = tokenize("自定义(8)'*''#' 说('*' / '#') 说(8 / '#') 说(8/'')");
  menu(line, "'*'");
  menu(line, "'#'");
  menu(line, "'*'", 1);
  menu(line, "'#'", 1);
  menu(line, "'#'", 2);
  lacks(line, '#', 'comment.');
  const [slots] = tokenize("(自定义(8)'*''#') <ext.模板<@ok>'#'>");
  menu(slots, "'*'");
  menu(slots, "'#'");
  const [compact] = tokenize("(8/'')");
  has(compact, '/', 'keyword.operator.sbdsl');
  menu(compact, "''");
  const multiline = tokenize("(\n '#'\n # between operands #\n /\n '*'\n)\n移动(1)步");
  menu(multiline[1], "'#'");
  has(multiline[2], 'between operands', 'comment.block.sbdsl');
  has(multiline[3], '/', 'keyword.operator.sbdsl');
  menu(multiline[4], "'*'");
  has(multiline[6], '移动', 'support.function.command.sbdsl');
  lacks(multiline[6], '移动', 'meta.block.reporter.');
  for (const source of ["(自定义(8)'*')", "(ext.自定义(8)'*')", "(自定义(8)'#')", "(ext.自定义(8)'a/b')"]) {
    const [menuSlot, following] = tokenize(source + '\n移动(1)步');
    menu(menuSlot, source.slice(source.indexOf("'"), source.lastIndexOf("'") + 1));
    has(menuSlot, ')', 'punctuation.definition.reporter.end.sbdsl');
    has(following, '移动', 'support.function.command.sbdsl');
    lacks(following, '移动', 'meta.block.reporter.');
  }
});

test("mod remains part of variable and parameter identifiers", () => {
  const [line] = tokenize('(7%3) (mod3+1) (amodb/2) (global:model*2) (@modCount-1)');
  has(line, "%", 'keyword.operator.sbdsl');
  has(line, '3', 'constant.numeric.sbdsl');
  for (const name of ['mod3', 'amodb', 'global:model']) {
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
  const [literal] = tokenize("说(\"+* -* ** /* > =*\") 说('*') 说('>')");
  has(literal, '+*', 'string.quoted.double.sbdsl');
  menu(literal, "'*'");
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
    '',
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
  const init = tokenize("init:\n    extensions: [\"a.js\" as \"wrong\"]\n移动(1)步");
  lacks(init[1], 'as', 'keyword.control.import.');
  lacks(init[1], 'wrong', 'entity.name.namespace.');
  has(init[2], '移动', 'support.function.command.sbdsl');
});

test('standalone build files and init use the same data grammar', () => {
  const fields = 'x: 10\nvisible: false\ncostumes: [{name: "猫", path: "cat.svg"}]';
  const config = tokenize(fields, buildGrammar);
  const init = tokenize("init:\n" + fields.split("\n").map(line => "    " + line).join("\n"));
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

test('first assignments infer scalar and list names without declaration keywords', () => {
  const rows = tokenize('score = 0\nlogs = {"a", 1}\nglobal:logs = {}\nstart:\n    logs = {}\n    score += 1');
  has(rows[0], 'score', 'variable.other.readwrite.sbdsl');
  for (const i of [1, 2, 4]) {
    has(rows[i], i === 2 ? 'global:logs' : 'logs', 'variable.other.array.sbdsl');
    has(rows[i], '{', 'punctuation.definition.list.begin.sbdsl');
  }
  for (const old of ['var score = 0', 'list logs = {}', 'global var score = 0']) {
    const [row] = tokenize(old);
    assert(row.tokens.some(t => t.scopes.includes('invalid.illegal.declaration.sbdsl')), old);
  }
});

test('qualified headers may begin with their first typed input', () => {
  const [row] = tokenize('ext.(x 坐标)达标:');
  has(row, 'ext', 'entity.name.namespace.sbdsl');
  assert(!row.tokens.some(t => t.scopes.some(s => s.startsWith('invalid.'))));
});
