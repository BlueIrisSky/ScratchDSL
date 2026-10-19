import * as assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import * as vscode from 'vscode';

async function eventually(check: () => boolean, description: string): Promise<void> {
  const deadline = Date.now() + 30000;
  while (!check()) {
    if (Date.now() > deadline) {
      const diagnostics = vscode.languages.getDiagnostics().map(([uri, items]) => ({
        uri: uri.toString(), messages: items.map(item => item.message)
      }));
      throw new Error('Timeout: ' + description + '\nDiagnostics: ' + JSON.stringify(diagnostics));
    }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
}

async function replace(document: vscode.TextDocument, text: string): Promise<void> {
  const edit = new vscode.WorkspaceEdit();
  edit.replace(document.uri, new vscode.Range(document.positionAt(0), document.positionAt(document.getText().length)), text);
  assert.equal(await vscode.workspace.applyEdit(edit), true);
}

async function chooseSymbol(document: vscode.TextDocument, marked: string, label: string): Promise<void> {
  const cursor = marked.indexOf('|');
  assert.ok(cursor >= 0);
  await replace(document, marked.replace('|', ''));
  const editor = await vscode.window.showTextDocument(document);
  const position = document.positionAt(cursor);
  editor.selection = new vscode.Selection(position, position);
  const candidates = await vscode.commands.executeCommand<vscode.CompletionList>(
    'vscode.executeCompletionItemProvider', document.uri, position);
  const candidate = candidates?.items.find(item =>
    (typeof item.label === 'string' ? item.label : item.label.label) === label);
  assert.ok(candidate, 'missing symbol completion: ' + label);
  assert.ok(candidate.range);
  const range = candidate.range instanceof vscode.Range ? candidate.range : candidate.range.replacing;
  if (candidate.insertText instanceof vscode.SnippetString) {
    assert.equal(await editor.insertSnippet(candidate.insertText, range), true);
    await vscode.commands.executeCommand('leaveSnippet');
  } else {
    const edit = new vscode.WorkspaceEdit();
    edit.replace(document.uri, range, candidate.insertText ?? label);
    assert.equal(await vscode.workspace.applyEdit(edit), true);
  }
}

export async function run(): Promise<void> {
  const workspace = process.env.SCRATCHDSL_SMOKE_WORKSPACE!;
  const trusted = process.env.SCRATCHDSL_SMOKE_MODE === 'trusted';
  assert.equal(vscode.workspace.isTrusted, trusted);
  // Opening only a build file must activate the extension and analyze its entry.
  const configUri = vscode.Uri.file(path.join(workspace, 'sbdsl.buildsb'));
  const config = await vscode.workspace.openTextDocument(configUri);
  await vscode.window.showTextDocument(config);
  assert.equal(config.languageId, 'sbdsl-build');
  assert.ok(!vscode.workspace.textDocuments.some(item => item.languageId === 'sbdsl'));
  const extension = vscode.extensions.getExtension('scratchdsl.scratchdsl');
  assert.ok(extension);
  await eventually(() => extension.isActive, 'config-only automatic activation');
  const api = extension.exports;
  await api.whenReady();
  const configOriginal = config.getText();
  await replace(config, 'entry:\n');
  await eventually(() => vscode.languages.getDiagnostics(configUri).length > 0, 'config-only unsaved diagnostic');
  await replace(config, configOriginal);
  await eventually(() => vscode.languages.getDiagnostics(configUri).length === 0, 'config-only diagnostic cleared');

  const sourceUri = vscode.Uri.file(path.join(workspace, 'src', 'main.sbdsl'));
  const document = await vscode.workspace.openTextDocument(sourceUri);
  await vscode.window.showTextDocument(document);
  assert.equal(document.languageId, 'sbdsl');

  const original = document.getText();
  await replace(document, original + '@unknown\n');
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length > 0, 'unsaved source diagnostic');
  const diagnostics = vscode.languages.getDiagnostics(sourceUri);
  assert.ok(diagnostics.some(item => item.severity === vscode.DiagnosticSeverity.Error));
  await replace(document, original);
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'source diagnostic cleared');

  await replace(document, original.replace('[将 $score 增加 1]', '[不存在的动作]\n        [说 $list:missing]') +
    '@var invalid = []\n@list alsoInvalid = [1,]\n');
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 4, 'multiple AST diagnostics');
  assert.ok(vscode.languages.getDiagnostics(sourceUri).every(item => item.severity === vscode.DiagnosticSeverity.Error));
  await replace(document, original);
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'all AST diagnostics cleared');

  const importedUri = vscode.Uri.file(path.join(workspace, 'src', 'module.sbdsl'));
  const importedOriginal = await fs.readFile(importedUri.fsPath, 'utf8');
  try {
    await fs.writeFile(importedUri.fsPath, importedOriginal + '@var invalid = []\n@list alsoInvalid = [1,]\n');
    await eventually(() => vscode.languages.getDiagnostics(importedUri).length === 2, 'unopened module AST diagnostics');
    assert.ok(!vscode.workspace.textDocuments.some(item => item.uri.toString() === importedUri.toString()));
    await replace(document, original.replace('import module\n', ''));
    await eventually(() => vscode.languages.getDiagnostics(importedUri).length === 0, 'removed import diagnostics cleared');
  } finally {
    await fs.writeFile(importedUri.fsPath, importedOriginal);
    await replace(document, original);
  }
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'project diagnostics cleared');

  const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
    'vscode.executeCompletionItemProvider', sourceUri, new vscode.Position(4, 8));
  assert.ok(completions && completions.items.length > 0, 'server completion items');

  const symbolSource = '@var score = 0\n@list "日志 😀" = []\n' +
    '定义自制积木 公共动作 { [等待 0 秒] }\n' +
    '@sprite "玩家" {\n @init {}\n @var hp = 100\n' +
    ' 定义自制积木 私有动作 { [等待 0 秒] }\n' +
    ' 当绿旗被点击 {\n  CALL\n }\n}\nimport module\n';
  await chooseSymbol(document, symbolSource.replace('CALL', '[说 $h|pSuffix]'), '$hp');
  assert.ok(document.getText().includes('[说 $hp]'), 'replace only the full variable reference');
  assert.ok(!document.getText().includes('Suffix'));
  await chooseSymbol(document, symbolSource.replace('CALL', '[说 $list:|]'), '$list:"日志 😀"');
  assert.ok(document.getText().includes('[说 $list:"日志 😀"]'), 'quoted list completion');
  await chooseSymbol(document, symbolSource.replace('CALL', '[私|]'), '[私有动作]');
  assert.ok(document.getText().includes('[私有动作]'), 'private procedure completion');
  await chooseSymbol(document, symbolSource.replace('CALL', '[公|]'), '[公共动作]');
  assert.ok(document.getText().includes('[公共动作]'), 'public procedure completion');
  await replace(document, original);
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'symbol edits cleared');

  const operatorCalls = [
    '[说 (1 + 2)]', '[说 (1-2)]', '[说 (2*3)]', '[说 (8 / 2)]',
    '[说 (7 mod 3)]', '[说 (8/(6/2))]', '[说 (-1.5e-2/-2e+3)]',
    '[等待直到 <$score<1>]', '[等待直到 <$score>100>]', '[等待直到 <$score=100>]',
    "[等待直到 <碰到 /'边缘'/ ?>]"
  ].join('\n        ');
  await replace(document, original.replace('[将 $score 增加 1]', '[说 (1 +* 2)]'));
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length > 0, 'retired arithmetic operator diagnostic');
  await replace(document, original.replace('[将 $score 增加 1]', operatorCalls));
  await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'ordinary comparison operators and quoted menus');
  for (const rejected of ['[等待直到 <$score >* 100>]', '[等待直到 <碰到 /边缘/ ?>]']) {
    await replace(document, original.replace('[将 $score 增加 1]', rejected));
    await eventually(() => vscode.languages.getDiagnostics(sourceUri).length > 0, 'retired syntax diagnostic: ' + rejected);
    await replace(document, original.replace('[将 $score 增加 1]', operatorCalls));
    await eventually(() => vscode.languages.getDiagnostics(sourceUri).length === 0, 'new syntax clears retired syntax diagnostic');
  }
  await replace(document, original);

  const untitled = await vscode.workspace.openTextDocument({ language: 'sbdsl', content: '@unknown\n' });
  await vscode.window.showTextDocument(untitled);
  await eventually(() => vscode.languages.getDiagnostics(untitled.uri).length > 0, 'untitled diagnostic');
  assert.equal(await vscode.commands.executeCommand('scratchdsl.build'), false);
  await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  await vscode.window.showTextDocument(document);

  if (!trusted) {
    assert.equal(await vscode.commands.executeCommand('scratchdsl.build'), false);
    assert.equal(await vscode.commands.executeCommand('scratchdsl.build', configUri), false);
    assert.equal(await vscode.commands.executeCommand('scratchdsl.check'), false);
    await assert.rejects(fs.access(path.join(workspace, 'dist', '工程.sb3')));
    console.log('PASS restricted workspace: bundled server, diagnostics, block and symbol completion, no build');
    return;
  }

  // An active module must use config.entry, saving both modified main and config.
  await replace(document, original + '# unsaved source saved by build\n');
  const changedConfig = configOriginal.replace('"./dist/工程.sb3"', '"./dist/已保存.sb3"');
  assert.notEqual(changedConfig, configOriginal);
  await replace(config, changedConfig);
  const module = await vscode.workspace.openTextDocument(vscode.Uri.file(path.join(workspace, 'src', 'module.sbdsl')));
  await vscode.window.showTextDocument(module);
  assert.equal(await vscode.commands.executeCommand('scratchdsl.build'), true);
  assert.equal(document.isDirty, false);
  assert.equal(config.isDirty, false);
  assert.ok((await fs.readFile(path.join(workspace, 'dist', '已保存.sb3'))).subarray(0, 2).equals(Buffer.from('PK')));
  assert.equal(await vscode.commands.executeCommand('scratchdsl.check'), true);

  // Editor-title actions supply the clicked URI, even if another editor is active.
  // A named build profile must also win over a nearby default sbdsl.buildsb.
  const profiles = path.join(workspace, 'profiles');
  await fs.mkdir(profiles);
  await fs.writeFile(path.join(profiles, 'sbdsl.buildsb'),
    'entry: "../src/main.sbdsl"\noutput: "../dist/自动配置.sb3"\n');
  const clickedUri = vscode.Uri.file(path.join(profiles, 'alternate.buildsb'));
  await fs.writeFile(clickedUri.fsPath,
    'entry: "../src/main.sbdsl"\noutput: "../dist/按钮旧值.sb3"\n');
  const clicked = await vscode.workspace.openTextDocument(clickedUri);
  assert.equal(clicked.languageId, 'sbdsl-build');
  await replace(clicked, clicked.getText().replace('按钮旧值', '按钮指定'));
  await vscode.window.showTextDocument(config);
  assert.equal(await vscode.commands.executeCommand('scratchdsl.build', clickedUri), true);
  assert.equal(clicked.isDirty, false);
  assert.ok((await fs.readFile(path.join(workspace, 'dist', '按钮指定.sb3'))).subarray(0, 2).equals(Buffer.from('PK')));
  await assert.rejects(fs.access(path.join(workspace, 'dist', '自动配置.sb3')));
  await assert.rejects(fs.access(path.join(workspace, 'dist', '按钮旧值.sb3')));
  await vscode.commands.executeCommand('scratchdsl.restartServer');
  await api.whenReady();
  assert.equal(vscode.languages.getDiagnostics(sourceUri).length, 0);
  console.log('PASS trusted workspace: AST diagnostics, block/symbol completion, config-only activation, unsaved build profiles, title-action URI, build/check and restart');
}
