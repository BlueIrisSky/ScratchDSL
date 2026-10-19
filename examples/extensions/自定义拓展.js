(function (Scratch) {
  class CustomExtension {
    getInfo() {
      return {
        id: 'custom',
        name: '自定义拓展',
        blocks: [
          {opcode: 'move', blockType: Scratch.BlockType.COMMAND, text: '移动 [STEPS] 步', arguments: {STEPS: {type: Scratch.ArgumentType.NUMBER, defaultValue: 10}}},
          {opcode: 'double', blockType: Scratch.BlockType.REPORTER, text: '[N] 的两倍', arguments: {N: {type: Scratch.ArgumentType.NUMBER, defaultValue: 1}}},
          {opcode: 'ready', blockType: Scratch.BlockType.BOOLEAN, text: '已就绪 ?'},
          {opcode: 'mode', blockType: Scratch.BlockType.COMMAND, text: '选择 [MODE]', arguments: {MODE: {type: Scratch.ArgumentType.STRING, menu: 'modes'}}}
        ],
        menus: {modes: {acceptReporters: false, items: [{text: '快速', value: 'fast'}, {text: '慢速', value: 'slow'}]}}
      };
    }
    move(args, util) { util.target.setXY(util.target.x + Number(args.STEPS), util.target.y); }
    double(args) { return Number(args.N) * 2; }
    ready() { return true; }
    mode(args) { this.selectedMode = args.MODE; }
  }
  Scratch.extensions.register(new CustomExtension());
})(Scratch);
