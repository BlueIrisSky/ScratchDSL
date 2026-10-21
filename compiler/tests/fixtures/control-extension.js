(function (Scratch) {
  class ControlExtension {
    getInfo() {
      return {
        id: 'flow',
        name: '扩展控制流测试',
        blocks: [
          {
            opcode: 'times', blockType: Scratch.BlockType.LOOP,
            text: '执行 [COUNT] 次',
            arguments: {COUNT: {type: Scratch.ArgumentType.NUMBER, defaultValue: 3}}
          },
          {
            opcode: 'choose', blockType: Scratch.BlockType.CONDITIONAL, branchCount: 2,
            text: ['如果 [CHECK]', '否则 [OTHER]'],
            arguments: {
              OTHER: {type: Scratch.ArgumentType.NUMBER, defaultValue: 7},
              CHECK: {type: Scratch.ArgumentType.BOOLEAN}
            }
          },
          {
            opcode: 'pick', blockType: Scratch.BlockType.CONDITIONAL, branchCount: 3,
            text: ['选择 [INDEX]', '第二 [LABEL]', '第三 [AMOUNT]', '结束 [TAIL]'],
            arguments: {
              TAIL: {type: Scratch.ArgumentType.STRING},
              AMOUNT: {type: Scratch.ArgumentType.NUMBER},
              LABEL: {type: Scratch.ArgumentType.STRING},
              INDEX: {type: Scratch.ArgumentType.NUMBER}
            }
          },
          {
            opcode: 'once', blockType: Scratch.BlockType.COMMAND, branchCount: 1,
            text: '单次 [VALUE]',
            arguments: {VALUE: {type: Scratch.ArgumentType.NUMBER}}
          }
        ]
      };
    }
    times(args, util) {
      if (util.stackFrame.remaining === undefined) {
        util.stackFrame.remaining = Math.floor(Number(args.COUNT));
      }
      if (util.stackFrame.remaining-- > 0) util.startBranch(1, true);
    }
    choose(args, util) {
      this.other = args.OTHER;
      util.startBranch(args.CHECK ? 1 : 2, false);
    }
    pick(args, util) {
      this.picked = [args.INDEX, args.LABEL, args.AMOUNT, args.TAIL];
      util.startBranch(Number(args.INDEX), false);
    }
    once(args, util) {
      this.onceValue = args.VALUE;
      util.startBranch(1, false);
    }
  }
  Scratch.extensions.register(new ControlExtension());
})(Scratch);
