const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { createWriteStream } = require('node:fs');
const { spawn } = require('node:child_process');
const { downloadAndUnzipVSCode, runTests } = require('@vscode/test-electron');

const source = "score = 0\nsprite \"玩家\":\n    init:\n        x: 0\n    end\n    start:\n        score += 1\n    end\nend\nimport module\n";

function launch(executable, args, env, log) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, { env, shell: false, windowsHide: true });
    child.stdout.pipe(log, { end: false });
    child.stderr.pipe(log, { end: false });
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error('VS Code test process exceeded 120 seconds.'));
    }, 120000);
    child.on('error', error => { clearTimeout(timer); reject(error); });
    child.on('close', code => {
      clearTimeout(timer);
      if (code === 0) resolve();
      else reject(new Error('VS Code exited with code ' + code));
    });
  });
}

async function cliScript(executable) {
  const root = path.dirname(executable);
  for (const candidate of [root, ...(await fs.readdir(root)).map(name => path.join(root, name))]) {
    const script = path.join(candidate, 'resources', 'app', 'out', 'cli.js');
    try { await fs.access(script); return script; } catch {}
  }
  throw new Error('Cannot find the installed VS Code CLI script.');
}

async function main() {
  if (process.platform !== 'win32' || process.arch !== 'x64') {
    throw new Error('Extension-host smoke requires Windows x64.');
  }
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'ScratchDSL VSCode 测试 '));
  const extensionRoot = path.resolve(__dirname, '..');
  const installed = process.argv[2] === '--installed';
  if (process.argv.length > 2 && (!installed || !process.argv[3])) {
    throw new Error('Usage: node test/run-host.cjs [--installed absolute-path.vsix]');
  }
  const vsix = installed ? path.resolve(process.argv[3]) : undefined;
  const requestedVersion = process.env.SCRATCHDSL_TEST_VSCODE_VERSION;
  if (requestedVersion && !/^\d+\.\d+\.\d+$/.test(requestedVersion)) {
    throw new Error('SCRATCHDSL_TEST_VSCODE_VERSION must be an exact VS Code version.');
  }
  // Local runs keep using the installed editor. CI explicitly opts into a fixed download.
  const executable = process.env.VSCODE_EXECUTABLE_PATH || (requestedVersion
    ? await downloadAndUnzipVSCode({
      version: requestedVersion,
      platform: 'win32-x64-archive',
      cachePath: path.join(extensionRoot, '.vscode-test'),
      timeout: 120000
    })
    : path.join(process.env.LOCALAPPDATA, 'Programs', 'Microsoft VS Code', 'Code.exe'));
  await fs.access(executable);
  await fs.access(vsix || path.join(extensionRoot, 'server', 'sbdsl.exe'));
  const logs = path.join(extensionRoot, '.vscode-test', 'logs');
  await fs.mkdir(logs, { recursive: true });
  try {
    for (const mode of ['trusted', 'untrusted']) {
      const workspace = path.join(root, mode, '中文工程');
      const profile = path.join(root, mode, 'profile');
      const extensions = path.join(root, mode, 'extensions');
      const logPath = path.join(logs, (installed ? 'installed-' : 'development-') + mode + '.log');
      const log = createWriteStream(logPath);
      await fs.mkdir(path.join(workspace, 'src'), { recursive: true });
      await fs.mkdir(path.join(workspace, '.vscode'), { recursive: true });
      await fs.mkdir(path.join(profile, 'User'), { recursive: true });
      await fs.writeFile(path.join(workspace, 'src', 'main.sbdsl'), source);
      // This module cannot build alone: its add depends on the entry creating the sprite first.
      await fs.writeFile(path.join(workspace, 'src', 'module.sbdsl'), "sprite \"玩家\" add:\n    当按下 'space' 键:\n        移动(1)步\n    end\nend\n");
      await fs.writeFile(path.join(workspace, 'sbdsl.buildsb'),
        '# 工程配置：裸字段名，换行分隔\nentry: "./src/main.sbdsl"\noutput: "./dist/工程.sb3"\nextensions: []\n');
      await fs.writeFile(path.join(profile, 'User', 'settings.json'), JSON.stringify({
        'security.workspace.trust.startupPrompt': 'never',
        'security.workspace.trust.untrustedFiles': 'open',
        'workbench.startupEditor': 'none',
        'extensions.autoUpdate': false,
        'update.mode': 'none'
      }));
      if (mode === 'untrusted') {
        await fs.writeFile(path.join(workspace, '.vscode', 'settings.json'), JSON.stringify({
          'scratchdsl.serverPath': 'C:\\ScratchDSL-do-not-execute\\untrusted.exe'
        }));
      }
      const env = { ...process.env, SCRATCHDSL_SMOKE_MODE: mode, SCRATCHDSL_SMOKE_WORKSPACE: workspace };
      delete env.ELECTRON_RUN_AS_NODE;
      let developmentPath = extensionRoot;
      if (installed) {
        await launch(executable, [
          await cliScript(executable), '--user-data-dir=' + profile,
          '--extensions-dir=' + extensions, '--install-extension', vsix, '--force'
        ], { ...env, ELECTRON_RUN_AS_NODE: '1' }, log);
        developmentPath = path.join(root, mode, 'harness');
        await fs.mkdir(developmentPath, { recursive: true });
        await fs.writeFile(path.join(developmentPath, 'package.json'), JSON.stringify({
          name: 'scratchdsl-smoke-harness', publisher: 'scratchdsl-test', version: '0.0.1',
          engines: { vscode: '^1.91.0' },
          capabilities: { untrustedWorkspaces: { supported: true } }
        }));
      }
      const options = {
        vscodeExecutablePath: executable,
        extensionDevelopmentPath: developmentPath,
        extensionTestsPath: path.join(extensionRoot, 'dist', 'test', 'index.js'),
        extensionTestsEnv: env,
        stdout: log, stderr: log,
        launchArgs: [
          workspace, '--user-data-dir=' + profile,
          '--extensions-dir=' + extensions,
          ...(!installed ? ['--disable-extensions'] : []),
          '--skip-welcome', '--skip-release-notes', '--disable-updates', '--disable-gpu-sandbox',
          '--no-sandbox', ...(mode === 'trusted' ? ['--disable-workspace-trust'] : [])
        ]
      };
      try {
        if (mode === 'trusted') {
          await runTests(options);
        } else {
          // test-electron forces --disable-workspace-trust. Launch directly to test Restricted Mode.
          await launch(executable, [
            ...options.launchArgs,
            '--extensionDevelopmentPath=' + developmentPath,
            '--extensionTestsPath=' + options.extensionTestsPath
          ], env, log);
        }
      } catch (error) {
        console.error('VS Code log: ' + logPath);
        throw error;
      } finally {
        await new Promise(resolve => log.end(resolve));
      }
      console.log('PASS extension-host ' + (installed ? 'installed ' : 'development ') + mode);
    }
  } finally {
    await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 250 });
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
