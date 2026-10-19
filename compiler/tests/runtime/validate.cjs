/* Development-only integration check against an unmodified TurboWarp VM checkout.
 * Usage: node compiler/tests/runtime/validate.cjs <scratch-vm-directory> [hello.sb3] [extensions.sb3] [procedures.sb3] [demo.sb3]
 * The Rust compiler and its installed executable do not depend on Node.
 */
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const nodeVM = require('node:vm');
const {createRequire} = require('node:module');

const vmDirectory = path.resolve(process.argv[2] || 'target/validation/scratch-vm');
const upstreamRequire = createRequire(path.join(vmDirectory, 'package.json'));
const VirtualMachine = upstreamRequire('./src/index.js');
const JSZip = upstreamRequire('@turbowarp/jszip');
const hello = path.resolve(process.argv[3] || 'examples/hello/dist/hello.sb3');
const extensions = path.resolve(process.argv[4] || 'examples/extensions/dist/extensions.sb3');

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function opcodeCounts(target) {
    const counts = {};
    for (const block of Object.values(target.blocks)) {
        // Top-level variables and lists have a compact array encoding.
        if (!block || Array.isArray(block)) continue;
        counts[block.opcode] = (counts[block.opcode] || 0) + 1;
    }
    return counts;
}

async function readProject(filename) {
    const bytes = fs.readFileSync(filename);
    const zip = await JSZip.loadAsync(bytes);
    const project = JSON.parse(await zip.file('project.json').async('string'));
    for (const target of project.targets) {
        for (const asset of [...target.costumes, ...target.sounds]) {
            const entry = zip.file(asset.md5ext);
            assert(entry, `missing packaged asset ${asset.md5ext}`);
            const data = await entry.async('nodebuffer');
            assert.equal(crypto.createHash('md5').update(data).digest('hex'), asset.assetId);
        }
    }
    return {bytes, project};
}

// The official unsandboxed loader inserts a <script> into a document. Supply
// that one host operation in Node, accepting only the project's embedded data
// URIs. VM registration, opcode dispatch and serialization remain upstream code.
function embeddedScriptHost(instances) {
    return {
        createElement(tag) {
            assert.equal(tag, 'script', `unexpected browser operation: ${tag}`);
            return {};
        },
        body: {
            appendChild(script) {
                const match = /^data:application\/javascript;charset=utf-8;base64,(.*)$/s.exec(script.src);
                assert(match, 'runtime test only loads embedded JavaScript');
                const register = global.Scratch.extensions.register;
                global.Scratch.extensions.register = extension => {
                    instances.set(extension.getInfo().id, extension);
                    register(extension);
                };
                nodeVM.runInThisContext(Buffer.from(match[1], 'base64').toString('utf8'), {
                    filename: 'embedded-extension.js', timeout: 1000,
                });
                return script;
            },
        },
    };
}

async function validate(filename, kind) {
    const {bytes, project} = await readProject(filename);
    const instances = new Map();
    const vm = new VirtualMachine();
    const spoken = [];
    vm.runtime.on('SAY', (_target, _type, message) => spoken.push(String(message)));
    vm.setCompilerOptions({enabled: false});
    vm.securityManager.getSandboxMode = () => 'unsandboxed';
    vm.securityManager.canLoadExtensionFromProject = url =>
        url.startsWith('data:application/javascript;charset=utf-8;base64,');
    const previousDocument = global.document;
    global.document = embeddedScriptHost(instances);
    try {
        await vm.loadProject(bytes);
        const serialized = JSON.parse(vm.toJSON());
        assert.equal(serialized.targets.length, project.targets.length);
        for (let i = 0; i < project.targets.length; i++) {
            assert.deepEqual(opcodeCounts(serialized.targets[i]), opcodeCounts(project.targets[i]),
                `${kind}: opcode graph changed during VM serialization`);
            if (!project.targets[i].isStage) {
                for (const property of ['x', 'y', 'direction', 'visible', 'rotationStyle', 'currentCostume']) {
                    assert.equal(serialized.targets[i][property], project.targets[i][property],
                        `${kind}: initial ${property} changed before the green flag`);
                }
            }
        }
        if (kind === 'demo') {
            assert.equal(serialized.targets[1].x, -100);
            assert.equal(serialized.targets[1].y, 0);
            assert.equal(serialized.targets[1].costumes[0].name, '蓝色笑脸');
        }
        if (kind === 'extensions') {
            assert.equal(serialized.extensionURLs.custom, project.extensionURLs.custom);
            const mode = Object.values(serialized.targets[1].blocks).find(block => block.opcode === 'custom_mode');
            assert.equal(mode.fields.MODE[0], 'fast');
        }
        vm.start();
        vm.greenFlag();
        const deadline = Date.now() + 6000;
        while (vm.runtime.threads.length && Date.now() < deadline) await sleep(20);
        assert.equal(vm.runtime.threads.length, 0, `${kind}: scripts did not finish`);
        const stage = vm.runtime.getTargetForStage();
        const sprite = vm.runtime.targets.find(target => !target.isStage && target.isOriginal);
        assert(sprite, 'missing sprite');
        if (kind === 'hello') {
            const score = Object.values(stage.variables).find(variable => variable.name === 'score');
            assert.equal(score.value, 10);
            assert.equal(sprite.x, 100);
        } else if (kind === 'extensions') {
            assert.equal(sprite.x, 30);
            assert(spoken.includes('6'), `reporter result was not spoken: ${JSON.stringify(spoken)}`);
            assert.equal(instances.get('custom').selectedMode, 'fast');
        } else if (kind === 'demo') {
            const score = Object.values(stage.variables).find(variable => variable.name === 'score');
            assert.equal(score.value, 20);
            assert.equal(sprite.x, 100);
            assert(spoken.includes('编译成功！已经走了 20 步。'));
        } else {
            const globalScore = Object.values(stage.variables).find(variable => variable.name === 'score');
            const privateScore = Object.values(sprite.variables).find(variable => variable.name === 'score');
            const logs = Object.values(stage.variables).find(variable => variable.name === 'logs');
            assert.equal(globalScore.value, 3);
            assert.equal(privateScore.value, 103);
            assert.deepEqual(logs.value, ['tick', 'tick', 'tick']);
        }
        return {project: kind, loaded: true, serialized: true, x: sprite.x,
            ...(kind === 'hello' ? {score: 10} : kind === 'extensions'
                ? {reporter: 6, mode: instances.get('custom').selectedMode}
                : kind === 'demo' ? {initialX: -100, score: 20}
                    : {globalScore: 3, privateScore: 103, recursiveCalls: 3})};
    } finally {
        vm.quit();
        if (previousDocument === undefined) delete global.document;
        else global.document = previousDocument;
    }
}

(async () => {
    const results = [];
    results.push(await validate(hello, 'hello'));
    results.push(await validate(extensions, 'extensions'));
    if (process.argv[5]) results.push(await validate(path.resolve(process.argv[5]), 'procedures'));
    if (process.argv[6]) results.push(await validate(path.resolve(process.argv[6]), 'demo'));
    console.log(JSON.stringify({runtime: vmDirectory, results}, null, 2));
})().catch(error => {
    console.error(error);
    process.exitCode = 1;
});
