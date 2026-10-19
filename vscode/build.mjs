import { build } from 'esbuild';
import fs from 'node:fs/promises';
import path from 'node:path';

const extension = await build({
  entryPoints: ['src/extension.ts'], bundle: true, outfile: 'dist/extension.js',
  platform: 'node', format: 'cjs', target: 'node20', external: ['vscode'],
  sourcemap: false, minify: false, legalComments: 'eof', metafile: true
});
const packages = new Map();
for (const input of Object.keys(extension.metafile.inputs)) {
  if (!input.includes('node_modules/')) continue;
  let directory = path.dirname(path.resolve(input));
  while (directory !== path.dirname(directory)) {
    try {
      const manifest = JSON.parse(await fs.readFile(path.join(directory, 'package.json'), 'utf8'));
      if (manifest.name && manifest.version) packages.set(manifest.name, { directory, manifest });
      break;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      directory = path.dirname(directory);
    }
  }
}
const notices = ['Third-party notices for the bundled ScratchDSL VS Code client.',
  'ScratchDSL is licensed under MIT (see LICENSE); the following licenses apply to their named dependencies.\n'];
for (const [name, { directory, manifest }] of [...packages].sort(([a], [b]) => a.localeCompare(b))) {
  const files = await fs.readdir(directory);
  const license = files.find(file => /^licen[cs]e(?:\.(?:txt|md))?$/i.test(file));
  if (!license) throw new Error('Missing license text for bundled dependency ' + name);
  notices.push('='.repeat(72), name + ' ' + manifest.version + ' (' + manifest.license + ')',
    await fs.readFile(path.join(directory, license), 'utf8'));
}
await fs.writeFile('THIRD_PARTY_NOTICES.txt', notices.join('\n'));
await build({
  entryPoints: ['test/host/index.ts'], bundle: true, outfile: 'dist/test/index.js',
  platform: 'node', format: 'cjs', target: 'node20', external: ['vscode'], sourcemap: false
});
