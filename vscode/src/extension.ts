import * as vscode from 'vscode';
import * as path from 'node:path';
import * as fs from 'node:fs/promises';
import { spawn, ChildProcess } from 'node:child_process';
import { LanguageClient } from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let transition: Promise<void> = Promise.resolve();
const builds = new Set<ChildProcess>();
const CONFIG_FILENAME = 'sbdsl.buildsb';

function isConfig(uri: vscode.Uri): boolean {
  return uri.scheme === 'file' && path.extname(uri.fsPath).toLowerCase() === '.buildsb';
}

function serverPath(context: vscode.ExtensionContext): string {
  const config = vscode.workspace.getConfiguration('scratchdsl');
  const setting = config.inspect<string>('serverPath');
  const configured = vscode.workspace.isTrusted
    ? config.get<string>('serverPath', '')
    : (setting?.globalValue ?? setting?.defaultValue ?? '');
  const executable = configured.trim() || context.asAbsolutePath('server/sbdsl.exe');
  if (!path.isAbsolute(executable)) {
    throw new Error('scratchdsl.serverPath 必须是绝对路径。');
  }
  return executable;
}

async function ensureServer(executable: string): Promise<void> {
  if (process.platform !== 'win32' || process.arch !== 'x64' || vscode.env.remoteName) {
    throw new Error('此版本只支持本地 Windows x64 工作区。');
  }
  if (!(await fs.stat(executable)).isFile()) {
    throw new Error(`语言服务器不是文件：${executable}`);
  }
}

function inside(file: string, root: string): boolean {
  const relative = path.relative(root, file);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

async function nearestConfig(start: string): Promise<string | undefined> {
  for (let directory = start; ; directory = path.dirname(directory)) {
    const candidate = path.join(directory, CONFIG_FILENAME);
    try {
      if ((await fs.stat(candidate)).isFile()) return candidate;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    if (path.dirname(directory) === directory) return undefined;
  }
}

async function commandInput(resource?: vscode.Uri): Promise<{ input: string; cwd: string }> {
  // Editor-title actions pass the URI of the clicked editor. The active editor
  // can belong to a different group by the time the command is handled.
  const document = resource
    ? await vscode.workspace.openTextDocument(resource)
    : vscode.window.activeTextEditor?.document;
  if (document && ['sbdsl', 'sbdsl-build'].includes(document.languageId) && document.uri.scheme !== 'file') {
    throw new Error('请先将入口或配置保存为本地文件。');
  }
  const active = document?.uri.scheme === 'file' &&
    ['sbdsl', 'sbdsl-build'].includes(document.languageId)
    ? document : undefined;
  const selectedConfig = active && isConfig(active.uri)
    ? active.uri.fsPath : undefined;
  const folders = vscode.workspace.workspaceFolders ?? [];
  let folder = active ? vscode.workspace.getWorkspaceFolder(active.uri) : undefined;
  if (!folder && !active && folders.length > 1) {
    folder = await vscode.window.showWorkspaceFolderPick();
    if (!folder) throw new Error('已取消工程选择。');
  }
  folder ??= !active ? folders[0] : undefined;
  if (!active && !folder) throw new Error('请先打开 .sbdsl、sbdsl.buildsb 文件或工程目录。');
  const start = active ? path.dirname(active.uri.fsPath) : folder!.uri.fsPath;
  const config = selectedConfig ?? await nearestConfig(start);
  const saveRoot = folder?.uri.fsPath ?? (config ? path.dirname(config) : start);
  const dirty = vscode.workspace.textDocuments.filter(doc =>
    doc.isDirty && doc.uri.scheme === 'file' &&
    (inside(doc.uri.fsPath, saveRoot) || doc.uri.fsPath === config));
  for (const doc of dirty) {
    if (!(await doc.save())) throw new Error(`文件未保存，已取消：${doc.uri.fsPath}`);
  }
  if (config) return { input: config, cwd: path.dirname(config) };
  if (active?.languageId === 'sbdsl') return { input: active.uri.fsPath, cwd: start };
  throw new Error('未找到 sbdsl.buildsb；请打开要构建的 .sbdsl 入口文件。');
}

export async function activate(context: vscode.ExtensionContext): Promise<{ whenReady(): Promise<void> }> {
  const output = vscode.window.createOutputChannel('ScratchDSL', { log: true });
  context.subscriptions.push(output);
  const watcher = vscode.workspace.createFileSystemWatcher('**/*.{sbdsl,buildsb,js}');
  context.subscriptions.push(watcher);

  const restart = (): Promise<void> => {
    transition = transition.catch(() => undefined).then(async () => {
      if (client) {
        const old = client;
        client = undefined;
        await old.stop();
      }
      const executable = serverPath(context);
      await ensureServer(executable);
      output.appendLine(`启动：${executable} lsp --stdio；工作区信任：${vscode.workspace.isTrusted}`);
      // Executable servers use stdio by default. Setting TransportKind.stdio
      // would append another --stdio to the explicit CLI arguments.
      const next = new LanguageClient('scratchdsl', 'ScratchDSL', {
        command: executable, args: ['lsp', '--stdio'],
        options: { shell: false }
      }, {
        documentSelector: [
          { scheme: 'file', language: 'sbdsl' },
          { scheme: 'untitled', language: 'sbdsl' },
          { scheme: 'file', language: 'sbdsl-build' }
        ],
        initializationOptions: { allowExtensions: vscode.workspace.isTrusted },
        synchronize: { fileEvents: watcher },
        outputChannel: output,
        markdown: { isTrusted: false }
      });
      client = next;
      try {
        await next.start();
      } catch (error) {
        client = undefined;
        await next.dispose();
        throw error;
      }
    });
    return transition;
  };
  const report = (error: unknown): void => {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(message);
    void vscode.window.showErrorMessage(`ScratchDSL：${message}`);
  };
  const run = async (command: 'build' | 'check', resource?: vscode.Uri): Promise<boolean> => {
    try {
      if (!vscode.workspace.isTrusted) throw new Error('请先信任工作区，再执行构建或检查。');
      const executable = serverPath(context);
      await ensureServer(executable);
      const { input, cwd } = await commandInput(resource);
      output.show(true);
      output.appendLine(`> ${JSON.stringify(executable)} ${command} ${JSON.stringify(input)}`);
      await new Promise<void>((resolve, reject) => {
        const child = spawn(executable, [command, input], {
          cwd, shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe']
        });
        builds.add(child);
        child.stdout?.setEncoding('utf8').on('data', text => output.append(text));
        child.stderr?.setEncoding('utf8').on('data', text => output.append(text));
        child.on('error', reject);
        child.on('close', code => {
          builds.delete(child);
          if (code === 0) resolve();
          else reject(new Error(`${command} 失败，退出码 ${code ?? '未知'}。`));
        });
      });
      return true;
    } catch (error) {
      report(error);
      return false;
    }
  };
  context.subscriptions.push(
    vscode.commands.registerCommand('scratchdsl.restartServer', () => restart().catch(report)),
    vscode.commands.registerCommand('scratchdsl.showOutput', () => output.show(true)),
    vscode.commands.registerCommand('scratchdsl.build', (resource?: vscode.Uri) => run('build', resource)),
    vscode.commands.registerCommand('scratchdsl.check', (resource?: vscode.Uri) => run('check', resource)),
    vscode.workspace.onDidGrantWorkspaceTrust(() => void restart().catch(report)),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('scratchdsl.serverPath')) void restart().catch(report);
    })
  );
  // LanguageClient synchronizes already-open documents after initialization.
  void restart().catch(report);
  return { whenReady: () => transition };
}

export async function deactivate(): Promise<void> {
  for (const child of builds) child.kill();
  await transition.catch(() => undefined);
  await client?.stop();
  client = undefined;
}
