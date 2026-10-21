//! Synchronous, local-only JavaScript metadata extraction. No Node or network is used.
use crate::config::{ExtensionSpec, local_path};
use crate::error::{Error, Result};
use crate::model::*;
use crate::registry::{Registry, input, tokens_from_text};
use rquickjs::{Context, Ctx, Function, Runtime};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

pub fn load_extensions(extensions: &[ExtensionSpec], registry: &mut Registry) -> Result<()> {
    let mut visited = BTreeMap::new();
    let mut names: BTreeSet<String> = registry
        .templates
        .iter()
        .filter_map(|t| t.namespace.clone())
        .collect();
    let mut used: BTreeSet<String> = registry
        .templates
        .iter()
        .map(|t| t.opcode.clone())
        .collect();
    used.extend(
        crate::builtins::RESERVED_OPCODES
            .iter()
            .map(|s| (*s).to_owned()),
    );
    for template in &registry.templates {
        for slot in template.slots() {
            if let Some(shadow) = &slot.shadow {
                used.insert(shadow.opcode.clone());
            }
        }
    }
    for extension in extensions {
        let path = &extension.path;
        local_path(path)?;
        let file = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| Error::new("扩展路径需要 UTF-8 文件名"))?;
        let filename_namespace = file
            .strip_suffix(".js")
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                Error::new(format!(
                    "扩展文件名必须具有非空名称和 .js 后缀: {}",
                    path.display()
                ))
            })?;
        if let Some(alias) = &extension.alias {
            crate::config::validate_extension_alias(alias).map_err(Error::new)?;
        }
        let namespace = extension.alias.as_deref().unwrap_or(filename_namespace);
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| Error::new(format!("扩展 {}: {e}", path.display())))?;
        if let Some(previous) = visited.get(&canonical) {
            if previous == namespace {
                continue;
            }
            return Err(Error::new(format!(
                "同一扩展文件不能使用不同命名空间 {previous:?} 和 {namespace:?}: {}",
                path.display()
            )));
        }
        visited.insert(canonical.clone(), namespace.to_owned());
        if !names.insert(namespace.to_owned()) {
            return Err(Error::new(format!(
                "扩展命名空间冲突 {namespace:?}: {}",
                path.display()
            )));
        }
        let source = std::fs::read_to_string(&canonical)
            .map_err(|e| Error::new(format!("读取扩展 {}: {e}", path.display())))?;
        let info = extract_info(&source, path)?;
        let id = string(&info, "id", "扩展")?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::new(format!(
                "扩展 {} 的 info.id 必须是非空 ASCII 字母/数字: {id:?}",
                path.display()
            )));
        }
        if registry.extension_sources.contains_key(id) {
            return Err(Error::new(format!(
                "扩展 info.id 冲突 {id:?}: {}",
                path.display()
            )));
        }
        let (templates, opcodes) = build_templates(&info, namespace)
            .map_err(|e| Error::new(format!("扩展 {}: {}", path.display(), e.0)))?;
        if let Some(opcode) = opcodes.iter().find(|op| used.contains(*op)) {
            return Err(Error::new(format!(
                "扩展 {} 的运行时 opcode 冲突: {opcode}",
                path.display()
            )));
        }
        used.extend(opcodes);
        registry.templates.extend(templates);
        registry.extension_sources.insert(id.to_owned(), source);
    }
    Ok(())
}

fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error, path: &Path, phase: &str) -> Error {
    let message = if matches!(error, rquickjs::Error::Exception) {
        let caught = ctx.catch();
        if let Some(exception) = caught.as_exception() {
            format!(
                "{}{}",
                exception.message().unwrap_or_default(),
                exception
                    .stack()
                    .map(|s| format!("\n{s}"))
                    .unwrap_or_default()
            )
        } else {
            format!("{caught:?}")
        }
    } else {
        error.to_string()
    };
    Error::new(format!("扩展 {} [{phase}]: {message}", path.display()))
}

fn extract_info(source: &str, path: &Path) -> Result<Value> {
    extract_info_with_timeout(source, path, Duration::from_secs(5))
}

fn extract_info_with_timeout(source: &str, path: &Path, timeout: Duration) -> Result<Value> {
    let runtime = Runtime::new().map_err(|e| Error::new(format!("创建 JavaScript 引擎: {e}")))?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + timeout;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context =
        Context::full(&runtime).map_err(|e| Error::new(format!("创建 JavaScript 上下文: {e}")))?;
    let owned = context.with(|ctx| -> Result<String> {
        let log = Function::new(ctx.clone(), |message: String| {
            eprintln!("[extension] {message}");
        })
        .map_err(|e| js_error(&ctx, e, path, "安装桩"))?;
        ctx.globals()
            .set("__sbdsl_log", log)
            .map_err(|e| js_error(&ctx, e, path, "安装桩"))?;
        ctx.eval::<(), _>(STUB)
            .map_err(|e| js_error(&ctx, e, path, "安装桩"))?;
        ctx.eval::<(), _>(source)
            .map_err(|e| js_error(&ctx, e, path, "同步执行"))?;
        ctx.eval::<String, _>("__sbdsl_take()")
            .map_err(|e| js_error(&ctx, e, path, "校验元信息"))
    })?;
    // Pending promise jobs are deliberately not run to finish asynchronous registration.
    if runtime.is_job_pending() {
        return Err(Error::new(format!(
            "扩展 {} 不支持异步初始化或异步注册",
            path.display()
        )));
    }
    Ok(serde_json::from_str(&owned)?)
}

// This projects validated metadata, rather than stringifying an extension instance.
// Required functions/undefined/non-finite values cannot disappear silently in JSON.
const STUB: &str = r#"
(() => {
  let count = 0;
  let info;
  const object = (v, label) => {
    if (!v || typeof v !== 'object' || Array.isArray(v) || typeof v.then === 'function')
      throw new Error(label + ' must be a synchronous object');
    return v;
  };
  const str = (v, label) => { if (typeof v !== 'string') throw new Error(label + ' must be a string'); return v; };
  const optionalBool = (v, label) => { if (v !== undefined && typeof v !== 'boolean') throw new Error(label + ' must be Boolean'); return v; };
  const scalar = (v, label) => {
    if (!['string', 'number', 'boolean'].includes(typeof v) || (typeof v === 'number' && !Number.isFinite(v)))
      throw new Error(label + ' must be a finite scalar');
    return v;
  };
  globalThis.window = globalThis;
  globalThis.self = globalThis;
  const log = (...args) => __sbdsl_log(args.map(v => String(v)).join(' '));
  globalThis.console = {log, info:log, warn:log, error:log, debug:log};
  globalThis.Scratch = {
    ArgumentType: {STRING:'string', NUMBER:'number', BOOLEAN:'Boolean', COLOR:'color', ANGLE:'angle', MATRIX:'matrix', NOTE:'note', IMAGE:'image', COSTUME:'costume', SOUND:'sound'},
    BlockType: {COMMAND:'command', REPORTER:'reporter', BOOLEAN:'Boolean', HAT:'hat', EVENT:'event', CONDITIONAL:'conditional', LOOP:'loop', LABEL:'label', BUTTON:'button'},
    TargetType: {SPRITE:'sprite', STAGE:'stage'},
    extensions: {unsandboxed:true, register(instance) {
      count++;
      if (count !== 1) throw new Error('exactly one extension registration is required');
      if (!instance || typeof instance.getInfo !== 'function') throw new Error('extension must define getInfo()');
      info = object(instance.getInfo(), 'getInfo()');
    }},
    translate: Object.assign(v => typeof v === 'string' ? v : (v && typeof v.default === 'string' ? v.default : ''), {setup() {}}),
    vm: {runtime: {on() {}}}
  };
  globalThis.__sbdsl_take = () => {
    if (count !== 1) throw new Error('exactly one synchronous extension registration is required');
    object(info, 'getInfo()');
    const out = {id:str(info.id, 'info.id'), blocks:[], menus:Object.create(null)};
    if (info.customFieldTypes !== undefined && Object.keys(object(info.customFieldTypes, 'info.customFieldTypes')).length)
      throw new Error('custom field types are unsupported');
    if (!Array.isArray(info.blocks)) throw new Error('info.blocks must be an array');
    for (const block of info.blocks) {
      if (block === '---') continue;
      object(block, 'block');
      if (block.blockType === 'label' || block.blockType === 'button') continue;
      const opcode = str(block.opcode, 'block.opcode');
      const blockType = str(block.blockType, opcode + '.blockType');
      const text = Array.isArray(block.text) ?
        block.text.map((part, index) => str(part, opcode + '.text[' + index + ']')) :
        str(block.text, opcode + '.text');
      const b = {opcode, blockType, text, arguments:Object.create(null)};
      const count = block.branchCount;
      if (count !== undefined && (!Number.isSafeInteger(count) || count < 0))
        throw new Error(opcode + '.branchCount must be a nonnegative safe integer');
      b.branchCount = blockType === 'loop' || blockType === 'conditional' ? (count || 1) : (count || 0);
      if (b.branchCount && !['loop', 'conditional', 'command'].includes(blockType))
        throw new Error(opcode + ': branch substacks require command, loop, or conditional blockType');
      if (block.branches !== undefined) throw new Error(opcode + ': use branchCount for substacks');
      if (optionalBool(block.isDynamic, opcode + '.isDynamic')) throw new Error(opcode + ': dynamic block mutations are unsupported');
      const targets = block.filter === undefined ? block.targetTypes : block.filter;
      if (targets !== undefined) {
        if (!Array.isArray(targets)) throw new Error(opcode + ': target filter must be an array');
        b.targets = targets.map(v => str(v, opcode + '.target'));
      }
      const args = block.arguments === undefined ? {} : object(block.arguments, opcode + '.arguments');
      for (const name of Object.keys(args)) {
        const arg = object(args[name], opcode + '.' + name);
        const a = {type:str(arg.type, opcode + '.' + name + '.type')};
        if (arg.menu !== undefined) a.menu = str(arg.menu, opcode + '.' + name + '.menu');
        if (arg.defaultValue !== undefined) a.defaultValue = scalar(arg.defaultValue, opcode + '.' + name + '.defaultValue');
        b.arguments[name] = a;
      }
      out.blocks.push(b);
    }
    const menus = info.menus === undefined ? {} : object(info.menus, 'info.menus');
    for (const name of Object.keys(menus)) {
      const raw = menus[name];
      const m = Array.isArray(raw) ? {items:raw, acceptReporters:false} : object(raw, 'menu ' + name);
      const clean = {acceptReporters:optionalBool(m.acceptReporters, 'menu acceptReporters') || false};
      if (typeof m.items === 'string') clean.items = m.items;
      else if (Array.isArray(m.items)) clean.items = m.items.map(item => {
        if (typeof item === 'string') return {text:item, value:item};
        object(item, 'menu item');
        return {text:str(item.text, 'menu item text'), value:str(item.value, 'menu item value')};
      });
      else throw new Error('menu ' + name + ': items must be an array or method name');
      out.menus[name] = clean;
    }
    return JSON.stringify(out);
  };
})();
"#;

fn string<'a>(value: &'a Value, key: &str, context: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new(format!("{context}.{key} 必须是字符串")))
}

fn build_templates(info: &Value, namespace: &str) -> Result<(Vec<Template>, BTreeSet<String>)> {
    let id = string(info, "id", "扩展")?;
    let menus = info
        .get("menus")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::new("缺少菜单元信息"))?;
    let blocks = info
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new("blocks 必须是数组"))?;
    let mut templates = Vec::new();
    let mut block_codes = BTreeSet::new();
    // The VM creates a block for every declared menu, even a field-only or unused menu.
    let mut menu_codes = BTreeSet::new();
    for name in menus.keys() {
        if name.contains(['<', '>', '&', '\'', '"']) {
            return Err(Error::new(format!(
                "菜单名称含运行时会转义的字符: {name:?}"
            )));
        }
        let mut validation_slot = input("menu", ArgType::String);
        configure_menu(&mut validation_slot, name, menus, id, None)?;
        menu_codes.insert(format!("{id}_menu_{name}"));
    }
    for block in blocks {
        let local = string(block, "opcode", "block")?;
        if local.is_empty() {
            return Err(Error::new("扩展 opcode 不能为空"));
        }
        if local.contains(['<', '"', '&']) {
            return Err(Error::new(format!(
                "扩展 opcode 含运行时会改写的字符: {local:?}"
            )));
        }
        let opcode = format!("{id}_{local}");
        if !block_codes.insert(opcode.clone()) {
            return Err(Error::new(format!("重复的扩展 opcode: {local}")));
        }
        let branch_count = block
            .get("branchCount")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| Error::new(format!("{local}: branchCount 必须是非负整数")))?;
        let shape = match string(block, "blockType", local)? {
            "command" if branch_count == 0 => Shape::Command,
            "command" | "loop" | "conditional" => Shape::Control,
            "reporter" => Shape::Reporter,
            "Boolean" => Shape::Boolean,
            "hat" | "event" => Shape::Hat,
            unsupported => {
                return Err(Error::new(format!("{local}: 不支持积木类型 {unsupported}")));
            }
        };
        let mut sprite_only = false;
        let mut stage_only = false;
        if let Some(targets) = block.get("targets") {
            let targets = targets
                .as_array()
                .ok_or_else(|| Error::new("目标类型应为数组"))?;
            let mut allowed = BTreeSet::new();
            for target in targets {
                match target.as_str() {
                    Some("sprite") => {
                        allowed.insert("sprite");
                    }
                    Some("stage") => {
                        allowed.insert("stage");
                    }
                    _ => return Err(Error::new(format!("{local}: 未知目标类型"))),
                }
            }
            if allowed.is_empty() {
                return Err(Error::new(format!("{local}: 目标类型不能为空")));
            }
            sprite_only = allowed.len() == 1 && allowed.contains("sprite");
            stage_only = allowed.len() == 1 && allowed.contains("stage");
        }
        let arguments = block
            .get("arguments")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::new(format!("{local}: arguments 必须是对象")))?;
        let mut slots = Vec::new();
        for (name, arg) in arguments {
            let branch_name = name == "SUBSTACK"
                || name
                    .strip_prefix("SUBSTACK")
                    .and_then(|index| index.parse::<usize>().ok())
                    .is_some_and(|index| {
                        index >= 2 && index <= branch_count && name == &format!("SUBSTACK{index}")
                    });
            if branch_count > 0 && branch_name {
                return Err(Error::new(format!(
                    "{local}.{name}: 参数名称与 C 型积木子堆栈输入冲突"
                )));
            }
            let kind = match string(arg, "type", &format!("{local}.{name}"))? {
                "string" => ArgType::String,
                "number" => ArgType::Number,
                "Boolean" => ArgType::Boolean,
                "color" => ArgType::Color,
                "angle" => ArgType::Angle,
                other => {
                    return Err(Error::new(format!(
                        "{local}.{name}: 不支持参数类型 {other}"
                    )));
                }
            };
            let mut slot = input(name, kind);
            if let Some(default) = arg.get("defaultValue") {
                slot.default = default.clone();
            }
            if let Some(menu_name) = arg.get("menu").and_then(Value::as_str) {
                if kind == ArgType::Boolean {
                    return Err(Error::new(format!("{local}.{name}: 布尔槽位不能作为菜单")));
                }
                configure_menu(&mut slot, menu_name, menus, id, arg.get("defaultValue"))
                    .map_err(|e| Error::new(format!("{local}.{name}: {}", e.0)))?;
                if let Some(shadow) = &slot.shadow {
                    menu_codes.insert(shadow.opcode.clone());
                }
            } else if kind == ArgType::Color
                && let Some(default) = arg.get("defaultValue")
            {
                let text = default.as_str().unwrap_or("");
                if text.len() != 7
                    || !text.starts_with('#')
                    || !text[1..].chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(Error::new(format!(
                        "{local}.{name}: 颜色默认值必须是 #rrggbb"
                    )));
                }
            }
            slots.push(slot);
        }
        let text = match &block["text"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .map(|part| {
                    part.as_str()
                        .ok_or_else(|| Error::new(format!("{local}: text 数组只能包含字符串")))
                })
                .collect::<Result<Vec<_>>>()?
                .join(" "),
            _ => {
                return Err(Error::new(format!(
                    "{local}: text 必须是字符串或字符串数组"
                )));
            }
        };
        let tokens =
            tokens_from_text(&text, &slots).map_err(|e| Error::new(format!("{local}: {}", e.0)))?;
        templates.push(Template {
            branch_count,
            opcode,
            shape,
            namespace: Some(namespace.into()),
            extension_id: Some(id.into()),
            local_opcode: Some(local.into()),
            tokens,
            sprite_only,
            stage_only,
            procedure: None,
            warp: false,
        });
    }
    if let Some(code) = block_codes.intersection(&menu_codes).next() {
        return Err(Error::new(format!(
            "积木 opcode 与菜单 shadow 冲突: {code}"
        )));
    }
    block_codes.extend(menu_codes);
    Ok((templates, block_codes))
}

fn configure_menu(
    slot: &mut Slot,
    name: &str,
    menus: &Map<String, Value>,
    id: &str,
    default: Option<&Value>,
) -> Result<()> {
    let info = menus
        .get(name)
        .ok_or_else(|| Error::new(format!("未知菜单 {name}")))?;
    let accept_reporters = info
        .get("acceptReporters")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let items = match info.get("items") {
        Some(Value::String(method)) if !method.is_empty() => None,
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut unique = Vec::new();
            for item in items {
                let pair = (
                    string(item, "text", "菜单项")?.to_owned(),
                    string(item, "value", "菜单项")?.to_owned(),
                );
                if !unique.contains(&pair) {
                    unique.push(pair);
                }
            }
            Some(unique)
        }
        _ => return Err(Error::new("菜单必须有非空静态选项或动态方法名")),
    };
    slot.default = match default {
        Some(Value::String(text)) => {
            if items
                .as_ref()
                .is_some_and(|items| !items.iter().any(|(_, value)| value == text))
            {
                return Err(Error::new(format!("菜单默认值 {text:?} 不在静态选项中")));
            }
            json!(text)
        }
        Some(_) => return Err(Error::new("菜单默认值必须是实际值字符串")),
        None => items
            .as_ref()
            .and_then(|v| v.first())
            .map(|(_, v)| json!(v))
            .unwrap_or(Value::Null),
    };
    slot.storage = if accept_reporters {
        Storage::Input
    } else {
        Storage::Field
    };
    slot.shadow = if accept_reporters {
        Some(Shadow {
            opcode: format!("{id}_menu_{name}"),
            field: name.into(),
            primitive: None,
        })
    } else {
        None
    };
    slot.menu = Some(Menu {
        items,
        accept_reporters,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn write_extension(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }
    const BASIC: &str = r#"Scratch.extensions.register({getInfo(){ return { id:'demo', blocks:[
      {opcode:'say',blockType:Scratch.BlockType.COMMAND,text:'记录 [TEXT]',arguments:{TEXT:{type:Scratch.ArgumentType.STRING}}},
      {opcode:'count',blockType:Scratch.BlockType.REPORTER,text:'数量'},
      {opcode:'ready',blockType:Scratch.BlockType.BOOLEAN,text:'已就绪 ?'},
      {opcode:'when',blockType:Scratch.BlockType.HAT,text:'完成 [VALUE]',arguments:{VALUE:{type:Scratch.ArgumentType.STRING}}},
      {opcode:'select',blockType:Scratch.BlockType.COMMAND,text:'选择 [SIDE]',arguments:{SIDE:{type:Scratch.ArgumentType.STRING,menu:'side'}}},
      {opcode:'clearA',blockType:Scratch.BlockType.COMMAND,text:'清空'},
      {opcode:'clearB',blockType:Scratch.BlockType.COMMAND,text:'清空'}
    ], menus:{side:{acceptReporters:false,items:[{text:'左',value:'left'}, {text:'右',value:'right'}]}} }; }});"#;
    #[test]
    fn loads_local_metadata_and_resolves_qualified_and_explicit_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(dir.path(), "自定义拓展.js", BASIC);
        let mut r = Registry::new();
        load_extensions(&[path.clone().into(), path.into()], &mut r).unwrap();
        assert_eq!(r.extension_sources.len(), 1);
        assert!(r.parse("记录 (3)", Shape::Command, &[]).is_err());
        let call = r
            .parse(
                "\"自定义拓展\".记录 (\"自定义拓展\".数量)",
                Shape::Command,
                &[],
            )
            .unwrap();
        assert_eq!(call.template.opcode, "demo_say");
        assert!(matches!(&call.args[0], Expr::Call(v) if v.template.opcode == "demo_count"));
        let call = r
            .parse("\"自定义拓展\".选择 '左'", Shape::Command, &[])
            .unwrap();
        assert_eq!(call.args, vec![Expr::Menu("left".into())]);
        assert_eq!(
            call.template.slots().next().unwrap().storage,
            Storage::Field
        );
        assert!(r.parse("\"自定义拓展\".清空", Shape::Command, &[]).is_err());
        assert_eq!(
            r.parse("\"自定义拓展\"::\"clearB\"()", Shape::Command, &[])
                .unwrap()
                .template
                .opcode,
            "demo_clearB"
        );
        assert!(
            r.parse("\"自定义拓展\"::\"when\"((\"ok\"))", Shape::Hat, &[])
                .is_ok()
        );
        assert!(
            r.parse("\"自定义拓展\"::\"say\"((1),)", Shape::Command, &[])
                .is_err()
        );
    }

    fn aliased(path: &Path, alias: &str) -> ExtensionSpec {
        ExtensionSpec {
            path: path.to_path_buf(),
            alias: Some(alias.into()),
        }
    }

    #[test]
    fn alias_replaces_filename_in_all_call_shapes_without_changing_runtime_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(dir.path(), "original-file.js", BASIC);
        let mut registry = Registry::new();
        let spec = aliased(&path, "自定义拓展.min-test");
        load_extensions(&[spec.clone(), spec], &mut registry).unwrap();
        assert_eq!(registry.extension_sources.len(), 1);
        assert_eq!(registry.extension_sources["demo"], BASIC);
        for (text, shape, opcode) in [
            (
                "自定义拓展.min-test.记录 (自定义拓展.min-test.数量)",
                Shape::Command,
                "demo_say",
            ),
            ("(自定义拓展.min-test.数量)", Shape::Reporter, "demo_count"),
            (
                "<自定义拓展.min-test.已就绪 ?>",
                Shape::Boolean,
                "demo_ready",
            ),
            (
                "自定义拓展.min-test.完成 (\"done\")",
                Shape::Hat,
                "demo_when",
            ),
            (
                "自定义拓展.min-test::\"when\"((\"done\"))",
                Shape::Hat,
                "demo_when",
            ),
            (
                "自定义拓展.min-test::\"clearA\"()",
                Shape::Command,
                "demo_clearA",
            ),
            (
                "\"自定义拓展.min-test\".选择 '左'",
                Shape::Command,
                "demo_select",
            ),
        ] {
            let call = registry
                .parse(text, shape, &[])
                .unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(call.template.opcode, opcode);
        }
        for old in ["(original-file.数量)", "(\"original-file\".数量)", "(数量)"] {
            assert!(registry.parse(old, Shape::Reporter, &[]).is_err(), "{old}");
        }
    }

    #[test]
    fn aliases_disambiguate_filenames_but_not_runtime_ids_or_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let first = write_extension(dir.path(), "demo.js", BASIC);
        let second = write_extension(&other, "demo.js", &BASIC.replace("id:'demo'", "id:'other'"));
        let mut registry = Registry::new();
        load_extensions(
            &[aliased(&first, "left"), aliased(&second, "right")],
            &mut registry,
        )
        .unwrap();
        assert_eq!(registry.extension_sources.len(), 2);
        assert_eq!(
            registry
                .parse("(left.数量)", Shape::Reporter, &[])
                .unwrap()
                .template
                .opcode,
            "demo_count"
        );
        assert_eq!(
            registry
                .parse("(right.数量)", Shape::Reporter, &[])
                .unwrap()
                .template
                .opcode,
            "other_count"
        );
        for specs in [
            vec![aliased(&first, "same"), aliased(&second, "same")],
            vec![first.clone().into(), aliased(&second, "demo")],
        ] {
            let error = load_extensions(&specs, &mut Registry::new()).unwrap_err();
            assert!(error.0.contains("命名空间冲突"), "{error}");
        }
        let same_id = write_extension(dir.path(), "same-id.js", BASIC);
        let error = load_extensions(
            &[aliased(&first, "one"), aliased(&same_id, "two")],
            &mut Registry::new(),
        )
        .unwrap_err();
        assert!(error.0.contains("info.id 冲突"), "{error}");
    }

    #[test]
    fn duplicate_sources_require_one_effective_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(dir.path(), "demo.js", BASIC);
        let mut registry = Registry::new();
        load_extensions(
            &[
                path.clone().into(),
                aliased(&dir.path().join("./demo.js"), "demo"),
            ],
            &mut registry,
        )
        .unwrap();
        assert_eq!(registry.extension_sources.len(), 1);
        for specs in [
            vec![path.clone().into(), aliased(&path, "renamed")],
            vec![aliased(&path, "first"), aliased(&path, "second")],
        ] {
            let error = load_extensions(&specs, &mut Registry::new()).unwrap_err();
            assert!(
                error.0.contains("同一扩展文件不能使用不同命名空间"),
                "{error}"
            );
        }
    }

    #[test]
    fn aliases_do_not_bypass_local_path_or_filename_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(dir.path(), "demo.js", BASIC);
        for alias in ["", " \t", "a\nb"] {
            assert!(load_extensions(&[aliased(&path, alias)], &mut Registry::new()).is_err());
        }
        let bad_name = write_extension(dir.path(), "demo.txt", BASIC);
        assert!(load_extensions(&[aliased(&bad_name, "valid")], &mut Registry::new()).is_err());
        for url in [
            "https://example.test/ext.js",
            "file:///ext.js",
            "data:text/javascript,foo",
        ] {
            let error = load_extensions(&[aliased(Path::new(url), "valid")], &mut Registry::new())
                .unwrap_err();
            assert!(error.0.contains("只支持本地"), "{error}");
        }
    }
    #[test]
    fn rejects_urls_async_and_unsupported_metadata() {
        let mut r = Registry::new();
        for path in [
            "https://example.com/ext.js",
            "file:///ext.js",
            "data:text/javascript,foo",
        ] {
            assert!(load_extensions(&[PathBuf::from(path).into()], &mut r).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        for (name, js) in [
            (
                "async.js",
                "Promise.resolve().then(() => Scratch.extensions.register({getInfo(){return {id:'x',blocks:[]}}}));",
            ),
            (
                "promise.js",
                "Scratch.extensions.register({getInfo(){return Promise.resolve({id:'x',blocks:[]})}});",
            ),
            (
                "matrix.js",
                "Scratch.extensions.register({getInfo(){return {id:'x',blocks:[{opcode:'x',blockType:'command',text:'[A]',arguments:{A:{type:'matrix'}}}]}}});",
            ),
            (
                "missing.js",
                "Scratch.extensions.register({getInfo(){return {id:'x',blocks:[{opcode:'x',blockType:'command',text:'[A]',arguments:{}}]}}});",
            ),
        ] {
            let p = write_extension(dir.path(), name, js);
            assert!(
                load_extensions(&[p.into()], &mut Registry::new()).is_err(),
                "{name}"
            );
        }
    }
    #[test]
    fn rejects_distinct_sources_with_same_namespace_or_runtime_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        let one = write_extension(dir.path(), "demo.js", BASIC);
        let same_name = write_extension(
            &dir.path().join("other"),
            "demo.js",
            &BASIC.replace("id:'demo'", "id:'other'"),
        );
        assert!(
            load_extensions(
                &[one.clone().into(), same_name.into()],
                &mut Registry::new()
            )
            .is_err()
        );
        let same_id = write_extension(dir.path(), "second.js", BASIC);
        assert!(load_extensions(&[one.into(), same_id.into()], &mut Registry::new()).is_err());
    }
    #[test]
    fn keeps_menu_dynamic_and_does_not_call_its_method() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(
            dir.path(),
            "dynamic.js",
            r#"Scratch.extensions.register({getInfo(){return {id:'dyn',blocks:[{opcode:'x',blockType:'command',text:'选 [A]',arguments:{A:{type:'string',menu:'m'}}}],menus:{m:{acceptReporters:true,items:'getItems'}}}}, getItems(){throw Error('must not run')}});"#,
        );
        let mut r = Registry::new();
        load_extensions(&[path.into()], &mut r).unwrap();
        let call = r
            .parse("dynamic.选 ('anything')", Shape::Command, &[])
            .unwrap();
        assert!(
            call.template
                .slots()
                .next()
                .unwrap()
                .menu
                .as_ref()
                .unwrap()
                .items
                .is_none()
        );
        assert_eq!(call.template.slots().next().unwrap().default, Value::Null);
    }

    #[test]
    fn validates_metadata_before_json_projection() {
        let path = Path::new("invalid.js");
        for info in [
            "{id:'x',blocks:[{opcode:'x',blockType:'command',text:()=> 'x'}]}",
            "{id:'x',blocks:[{opcode:'x',blockType:'command',text:['x', 1]}]}",
            "{id:'x',blocks:[{opcode:'x',blockType:'command',text:'[A]',arguments:{A:{type:'string',defaultValue:()=> 'x'}}}]}",
            "{id:'x',blocks:[],menus:{m:{items:()=> ['x']}}}",
            "{id:'x',blocks:[],customFieldTypes:{special:{implementation(){}}}}",
            "{id:'x',blocks:[{opcode:'x',blockType:'command',text:'x',isDynamic:true}]}",
        ] {
            let source = format!("Scratch.extensions.register({{getInfo(){{return {info}}}}});");
            assert!(extract_info(&source, path).is_err(), "{info}");
        }
    }

    #[test]
    fn control_blocks_flatten_all_text_arms_and_preserve_slot_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_extension(
            dir.path(),
            "control.js",
            r#"Scratch.extensions.register({getInfo(){return {id:'flow',blocks:[
                {opcode:'multi',blockType:'conditional',branchCount:3,
                 text:['若 [A]','否则 [B]','再次 [C]','尾部 [D]','结束 [E]'],
                 arguments:{E:{type:'number'},C:{type:'string'},A:{type:'Boolean'},
                    D:{type:'string',menu:'side'},B:{type:'number'}}},
                {opcode:'loop',blockType:'loop',text:'循环 [N]',arguments:{N:{type:'number'}}},
                {opcode:'zero',blockType:'conditional',branchCount:0,text:'默认分支'},
                {opcode:'command',blockType:'command',branchCount:5,text:'多个分支'},
                {opcode:'ordinary',blockType:'command',text:['前 [A]','后 [B]'],
                 arguments:{B:{type:'number'},A:{type:'number'}}},
                {opcode:'empty',blockType:'conditional',branchCount:2,text:[]}
            ],menus:{side:['left','right']}}}});"#,
        );
        let mut registry = Registry::new();
        load_extensions(&[path.into()], &mut registry).unwrap();
        for source in [
            r#"control.若 <true> 否则 (2) 再次 ("c") 尾部 'left' 结束 (5)"#,
            r#"control::"multi"(<true>, (2), ("c"), 'left', (5))"#,
        ] {
            let call = registry.parse(source, Shape::Control, &[]).unwrap();
            assert_eq!(call.template.opcode, "flow_multi");
            assert_eq!(call.template.branch_count, 3);
            assert_eq!(
                call.template
                    .slots()
                    .map(|slot| slot.name.as_str())
                    .collect::<Vec<_>>(),
                ["A", "B", "C", "D", "E"]
            );
            assert_eq!(
                call.args,
                [
                    Expr::Bool(true),
                    Expr::Number(2.),
                    Expr::String("c".into()),
                    Expr::Menu("left".into()),
                    Expr::Number(5.),
                ]
            );
            assert!(registry.parse(source, Shape::Command, &[]).is_err());
        }
        for (source, branches) in [
            ("control.循环 (2)", 1),
            ("control.默认分支", 1),
            ("control.多个分支", 5),
            ("control::\"empty\"()", 2),
        ] {
            assert_eq!(
                registry
                    .parse(source, Shape::Control, &[])
                    .unwrap()
                    .template
                    .branch_count,
                branches
            );
        }
        let ordinary = registry
            .parse("control.前 (1) 后 (2)", Shape::Command, &[])
            .unwrap();
        assert_eq!(ordinary.template.branch_count, 0);
        assert_eq!(ordinary.args, [Expr::Number(1.), Expr::Number(2.)]);
        for source in [
            "control.循环 2",
            r#"control::"multi"(<true>, (2))"#,
            "control.前 (1) 后 (2)",
        ] {
            assert!(
                registry.parse(source, Shape::Control, &[]).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn controls_validate_branch_counts_and_substack_name_collisions() {
        let path = Path::new("invalid.js");
        for count in [
            "-1",
            "1.5",
            "'2'",
            "null",
            "true",
            "NaN",
            "Infinity",
            "9007199254740992",
        ] {
            let source = format!(
                "Scratch.extensions.register({{getInfo(){{return {{id:'x',blocks:[{{opcode:'c',blockType:'loop',text:'x',branchCount:{count}}}]}}}}}});"
            );
            assert!(extract_info(&source, path).is_err(), "{count}");
        }
        for kind in ["reporter", "Boolean", "hat", "event"] {
            let source = format!(
                "Scratch.extensions.register({{getInfo(){{return {{id:'x',blocks:[{{opcode:'c',blockType:'{kind}',text:'x',branchCount:1}}]}}}}}});"
            );
            assert!(extract_info(&source, path).is_err(), "{kind}");
        }
        let dir = tempfile::tempdir().unwrap();
        for name in ["SUBSTACK", "SUBSTACK2", "SUBSTACK3", "SUBSTACK4"] {
            let source = format!(
                "Scratch.extensions.register({{getInfo(){{return {{id:'x',blocks:[{{opcode:'c',blockType:'conditional',branchCount:3,text:'[{name}]',arguments:{{{name}:{{type:'number'}}}}}}]}}}}}});"
            );
            let path = write_extension(dir.path(), "collision.js", &source);
            let result = load_extensions(&[path.into()], &mut Registry::new());
            assert_eq!(result.is_ok(), name == "SUBSTACK4", "{name}: {result:?}");
        }
    }

    #[test]
    fn rejects_all_menu_and_core_opcode_collisions() {
        let dir = tempfile::tempdir().unwrap();
        for (name, info) in [
            (
                "unused",
                "{id:'x',blocks:[{opcode:'menu_m',blockType:'command',text:'x'}],menus:{m:['x']}}",
            ),
            (
                "field",
                "{id:'x',blocks:[{opcode:'menu_m',blockType:'command',text:'x'},{opcode:'choose',blockType:'command',text:'[A]',arguments:{A:{type:'string',menu:'m'}}}],menus:{m:['x']}}",
            ),
            (
                "sound",
                "{id:'sound',blocks:[{opcode:'play',blockType:'command',text:'x'}]}",
            ),
            (
                "math",
                "{id:'math',blocks:[{opcode:'number',blockType:'reporter',text:'x'}]}",
            ),
            (
                "sanitized",
                "{id:'x',blocks:[{opcode:'a&b',blockType:'command',text:'x'}]}",
            ),
        ] {
            let source = format!("Scratch.extensions.register({{getInfo(){{return {info}}}}});");
            let path = write_extension(dir.path(), &format!("{name}.js"), &source);
            assert!(
                load_extensions(&[path.into()], &mut Registry::new()).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn interrupts_nonterminating_initialization() {
        let error = extract_info_with_timeout(
            "while (true) {}",
            Path::new("loop.js"),
            Duration::from_millis(25),
        )
        .unwrap_err();
        assert!(error.0.contains("interrupted"), "{}", error.0);
    }
}
