//! Semantic binding and native Scratch 3 block-graph generation.
use crate::analysis::bind::{BoundProcedure, BoundProject, BoundStatement, called_procedures};
#[cfg(test)]
use crate::registry::Registry;
use crate::{Error, Result, model::*};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Compile a merged project. Asset population and extension source embedding are
/// deliberately owned by the packaging layer.
#[cfg(test)]
pub(crate) fn generate(project: &Project, registry: &Registry) -> Result<GeneratedProject> {
    let analysis = crate::analysis::bind::bind_project(project, registry)?;
    generate_bound(&analysis)
}

pub(crate) fn generate_bound(analysis: &BoundProject) -> Result<GeneratedProject> {
    let procedures = &analysis.procedures;
    let scripts = &analysis.scripts;
    let source_metadata: Vec<Value> = analysis.procedure_order.iter().map(|id| {
        let procedure = &procedures[id];
        json!({
            "symbolId": id, "scope": procedure.scope.map(target_key).unwrap_or_else(|| "public".into()),
            "template": procedure.source.template, "span": procedure.source.span,
            "calls": called_procedures(&procedure.body), "body": bound_body_json(&procedure.body),
        })
    }).collect();
    let mut broadcasts = BTreeMap::new();
    let mut extension_ids = BTreeSet::new();
    let mut instances_metadata = Vec::new();
    let mut target_json = Vec::new();
    for (index, target) in analysis.targets.iter().enumerate() {
        let instances: BTreeMap<String, Instance> = analysis.selected[index]
            .iter()
            .cloned()
            .map(|id| {
                let instance = Instance::new(index, &id, &procedures[&id].source.template);
                (id, instance)
            })
            .collect();
        let mut emitter = Emitter {
            target: index,
            blocks: Map::new(),
            sequence: 0,
            instances,
            broadcasts: &mut broadcasts,
            extensions: &mut extension_ids,
            calls: Vec::new(),
        };
        for (script_index, script) in scripts[index].iter().enumerate() {
            if let Some(hat) = &script.hat {
                let hat = emitter.invocation(hat, None, &script.span)?;
                emitter.top_level(&hat, script_index);
                let first = emitter.stack(&script.body, Some(&hat))?;
                emitter.blocks[&hat]["next"] = json!(first);
            } else if let Some(first) = emitter.stack(&script.body, None)? {
                emitter.top_level(&first, script_index);
            }
        }
        let instance_ids: Vec<String> = emitter.instances.keys().cloned().collect();
        for (offset, id) in instance_ids.iter().enumerate() {
            emitter.procedure(&procedures[id], id, scripts[index].len() + offset)?;
        }
        for (id, instance) in &emitter.instances {
            instances_metadata.push(json!({
                "symbolId": id, "target": target_key(index), "targetName": target.name,
                "proccode": instance.proccode, "argumentIds": instance.argument_ids,
            }));
        }
        let call_metadata = std::mem::take(&mut emitter.calls);
        let mut value = json!({
            "isStage": index == 0,
            "name": if index == 0 && target.name.is_empty() {"Stage"} else {&target.name},
            "variables": data_json(index, &target.variables, false),
            "lists": data_json(index, &target.lists, true),
            "broadcasts": {}, "blocks": emitter.blocks, "comments": {},
            "currentCostume": 0, "costumes": [], "sounds": [], "volume": 100,
            "layerOrder": index,
        });
        if index == 0 {
            value["tempo"] = json!(60);
            value["videoTransparency"] = json!(50);
            value["videoState"] = json!("on");
            value["textToSpeechLanguage"] = Value::Null;
        } else {
            value["visible"] = json!(true);
            value["x"] = json!(0);
            value["y"] = json!(0);
            value["size"] = json!(100);
            value["direction"] = json!(90);
            value["draggable"] = json!(false);
            value["rotationStyle"] = json!("all around");
        }
        target_json.push(value);
        instances_metadata.push(json!({"target": target_key(index), "calls": call_metadata}));
    }
    target_json[0]["broadcasts"] = json!(broadcasts);
    Ok(GeneratedProject {
        json: json!({
            "targets": target_json, "monitors": [], "extensions": extension_ids,
            "meta": {"semver": "3.0.0", "vm": "0.2.0", "agent": "ScratchDSL",
                "scratchDSL": {"version": 1, "procedures": source_metadata, "instances": instances_metadata}},
        }),
        assets: BTreeMap::new(),
    })
}

fn target_key(index: usize) -> String {
    if index == 0 {
        "stage".into()
    } else {
        format!("sprite:{index}")
    }
}

fn escaped_id(text: &str) -> String {
    text.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn symbol_id(target: usize, name: &str, list: bool) -> String {
    format!(
        "t{target}_{}_{}",
        if list { "l" } else { "v" },
        escaped_id(name)
    )
}

fn data_json(target: usize, data: &BTreeMap<String, Value>, list: bool) -> Value {
    Value::Object(
        data.iter()
            .map(|(name, value)| (symbol_id(target, name, list), json!([name, value])))
            .collect(),
    )
}

fn bound_body_json(body: &[BoundStatement]) -> Value {
    json!(
        body.iter()
            .map(|statement| json!({
                "invocation": statement.invocation, "span": statement.span,
                "branches": statement.branches.iter()
                    .map(|branch| bound_body_json(branch)).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>()
    )
}

#[derive(Clone)]
struct Instance {
    proccode: String,
    argument_ids: Vec<String>,
    warp: bool,
}

impl Instance {
    fn new(target: usize, id: &str, template: &Template) -> Self {
        // Percent signs in source labels must never become runtime placeholders.
        let text = template
            .tokens
            .iter()
            .map(|token| match token {
                Token::Literal(text) => text.replace('%', "％"),
                Token::Slot(slot) => {
                    if slot.arg_type == ArgType::Boolean {
                        "%b".into()
                    } else {
                        "%s".into()
                    }
                }
            })
            .collect::<String>();
        Self {
            proccode: format!("{} [sbdsl:{}]", text.trim(), escaped_id(id)),
            argument_ids: template
                .slots()
                .enumerate()
                .map(|(i, _)| format!("t{target}_p{}_a{i}", escaped_id(id)))
                .collect(),
            warp: template.warp,
        }
    }
    fn mutation(&self) -> Value {
        json!({"tagName": "mutation", "children": [], "proccode": self.proccode,
            "argumentids": serde_json::to_string(&self.argument_ids).unwrap(), "warp": self.warp.to_string()})
    }
}

struct Emitter<'a> {
    target: usize,
    blocks: Map<String, Value>,
    sequence: usize,
    instances: BTreeMap<String, Instance>,
    broadcasts: &'a mut BTreeMap<String, String>,
    extensions: &'a mut BTreeSet<String>,
    calls: Vec<Value>,
}

impl Emitter<'_> {
    fn block(&mut self, opcode: &str, parent: Option<&str>, shadow: bool) -> String {
        self.sequence += 1;
        let id = format!("t{}_b{}", self.target, self.sequence);
        self.blocks.insert(
            id.clone(),
            json!({
                "opcode": opcode, "next": null, "parent": parent,
                "inputs": {}, "fields": {}, "shadow": shadow, "topLevel": false,
            }),
        );
        id
    }

    fn top_level(&mut self, id: &str, index: usize) {
        self.blocks[id]["topLevel"] = json!(true);
        self.blocks[id]["x"] = json!(40 + (index % 4) * 350);
        self.blocks[id]["y"] = json!(40 + (index / 4) * 250);
    }

    fn stack(&mut self, body: &[BoundStatement], parent: Option<&str>) -> Result<Option<String>> {
        let mut first = None;
        let mut previous: Option<String> = None;
        for statement in body {
            let id = self.invocation(
                &statement.invocation,
                previous.as_deref().or(parent),
                &statement.span,
            )?;
            if first.is_none() {
                first = Some(id.clone());
            }
            if let Some(previous) = previous {
                self.blocks[&previous]["next"] = json!(id);
            }
            for (index, branch) in statement.branches.iter().enumerate() {
                let first = self.stack(branch, Some(&id))?;
                let input = if index == 0 {
                    "SUBSTACK".to_owned()
                } else {
                    format!("SUBSTACK{}", index + 1)
                };
                self.blocks[&id]["inputs"][&input] = json!([2, first]);
            }
            previous = Some(id);
        }
        Ok(first)
    }

    fn invocation(
        &mut self,
        invocation: &Invocation,
        parent: Option<&str>,
        span: &Span,
    ) -> Result<String> {
        let template = &invocation.template;
        if let Some(extension) = &template.extension_id {
            self.extensions.insert(extension.clone());
        }
        let procedure = template
            .procedure
            .as_ref()
            .map(|id| {
                self.instances
                    .get(id)
                    .cloned()
                    .ok_or_else(|| Error::new(format!("过程 {id:?} 没有当前目标实例")).at(span))
            })
            .transpose()?;
        let id = self.block(
            if procedure.is_some() {
                "procedures_call"
            } else {
                &template.opcode
            },
            parent,
            false,
        );
        if let Some(instance) = &procedure {
            self.blocks[&id]["mutation"] = instance.mutation();
            self.calls.push(json!({"blockId": id, "symbolId": template.procedure, "proccode": instance.proccode}));
        }
        for (i, (slot, value)) in template.slots().zip(&invocation.args).enumerate() {
            let name = procedure
                .as_ref()
                .map(|p| p.argument_ids[i].as_str())
                .unwrap_or(&slot.name);
            if slot.storage == Storage::Field {
                let field = self.field(slot, value).map_err(|e| e.at(span))?;
                self.blocks[&id]["fields"][name] = field;
            } else if let Some(input) = self.input(slot, value, &id, span)? {
                self.blocks[&id]["inputs"][name] = input;
            }
        }
        // Scratch uses this mutation to distinguish the stack/cap variants.
        if template.opcode == "control_stop" {
            let others = invocation.args.iter().any(|v| matches!(v, Expr::Menu(s) if s == "other scripts in sprite" || s == "角色的其他脚本"));
            self.blocks[&id]["mutation"] =
                json!({"tagName": "mutation", "children": [], "hasnext": others.to_string()});
        }
        Ok(id)
    }

    fn field(&mut self, slot: &Slot, value: &Expr) -> Result<Value> {
        match slot.arg_type {
            ArgType::Variable | ArgType::List => {
                if let Expr::Symbol { name, list, global } = value {
                    if *list != (slot.arg_type == ArgType::List) {
                        return Err(Error::new("字段的变量/列表类别不符"));
                    }
                    Ok(json!([
                        name,
                        symbol_id(if *global { 0 } else { self.target }, name, *list)
                    ]))
                } else {
                    Err(Error::new("变量或列表字段必须使用符号引用"))
                }
            }
            ArgType::Broadcast => {
                let name =
                    scalar_text(value).ok_or_else(|| Error::new("接收广播字段必须是广播名称"))?;
                let id = self.broadcast(&name);
                Ok(json!([name, id]))
            }
            _ => Ok(json!([
                scalar_text(value).ok_or_else(|| Error::new("下拉字段必须是菜单字面量"))?,
                null
            ])),
        }
    }

    fn broadcast(&mut self, name: &str) -> String {
        let id = format!("broadcast_{}", escaped_id(name));
        self.broadcasts.insert(id.clone(), name.into());
        id
    }

    fn shadow(&mut self, slot: &Slot, value: &Value, parent: &str) -> Option<Value> {
        if slot.arg_type == ArgType::Broadcast {
            let name = json_text(value);
            let id = self.broadcast(&name);
            return Some(json!([11, name, id]));
        }
        let spec = slot.shadow.as_ref()?;
        if let Some(tag) = spec.primitive {
            return Some(json!([tag, json_text(value)]));
        }
        let id = self.block(&spec.opcode, Some(parent), true);
        self.blocks[&id]["fields"][&spec.field] = json!([json_text(value), null]);
        Some(json!(id))
    }

    fn default_value(slot: &Slot) -> Option<Value> {
        if !slot.default.is_null() {
            return Some(slot.default.clone());
        }
        if let Some(menu) = &slot.menu {
            return menu
                .items
                .as_ref()
                .and_then(|items| items.first())
                .map(|(_, value)| json!(value));
        }
        match slot.arg_type {
            ArgType::Boolean | ArgType::Variable | ArgType::List => None,
            ArgType::Number | ArgType::Angle => Some(json!(0)),
            ArgType::Color => Some(json!("#000000")),
            _ => Some(json!("")),
        }
    }

    fn input(
        &mut self,
        slot: &Slot,
        value: &Expr,
        parent: &str,
        span: &Span,
    ) -> Result<Option<Value>> {
        if matches!(value, Expr::EmptyBool) {
            return if slot.arg_type == ArgType::Boolean {
                Ok(None)
            } else {
                Err(Error::new("未连接布尔槽 <> 不能作为普通值").at(span))
            };
        }
        let is_menu = slot.menu.is_some();
        // Broadcast text denotes a selected message, not a text reporter.
        let literal = if slot.arg_type == ArgType::Broadcast {
            scalar_text(value).map(Value::String)
        } else if is_menu {
            if let Expr::Menu(text) = value {
                Some(json!(text))
            } else {
                None
            }
        } else {
            match value {
                Expr::Number(n) => Some(json!(n.to_string())),
                Expr::String(s) | Expr::Menu(s) => Some(json!(s)),
                _ => None,
            }
        };
        if let Some(literal) = literal {
            if let Some(shadow) = self.shadow(slot, &literal, parent) {
                return Ok(Some(json!([1, shadow])));
            }
            let primitive = scalar_primitive(value)
                .ok_or_else(|| Error::new("此输入没有可编码的字面量形状").at(span))?;
            return Ok(Some(json!([1, primitive])));
        }
        let child = self.expression(value, parent, span)?;
        let shadow = Self::default_value(slot).and_then(|value| self.shadow(slot, &value, parent));
        Ok(Some(if let Some(shadow) = shadow {
            json!([3, child, shadow])
        } else {
            json!([2, child])
        }))
    }

    fn expression(&mut self, value: &Expr, parent: &str, span: &Span) -> Result<Value> {
        Ok(match value {
            Expr::Symbol { name, list, global } => json!([
                if *list { 13 } else { 12 },
                name,
                symbol_id(if *global { 0 } else { self.target }, name, *list)
            ]),
            Expr::Param { name, boolean } => {
                let id = self.block(
                    if *boolean {
                        "argument_reporter_boolean"
                    } else {
                        "argument_reporter_string_number"
                    },
                    Some(parent),
                    false,
                );
                self.blocks[&id]["fields"]["VALUE"] = json!([name, null]);
                json!(id)
            }
            Expr::Call(call) => json!(self.invocation(call, Some(parent), span)?),
            Expr::Bool(value) => {
                let id = self.block("operator_equals", Some(parent), false);
                self.blocks[&id]["inputs"]["OPERAND1"] = json!([1, [10, "1"]]);
                self.blocks[&id]["inputs"]["OPERAND2"] =
                    json!([1, [10, if *value { "1" } else { "0" }]]);
                json!(id)
            }
            Expr::EmptyBool => return Err(Error::new("未连接布尔槽不能独立求值").at(span)),
            _ => {
                scalar_primitive(value).ok_or_else(|| Error::new("无法编码输入表达式").at(span))?
            }
        })
    }

    fn procedure(
        &mut self,
        procedure: &BoundProcedure,
        symbol: &str,
        position: usize,
    ) -> Result<()> {
        let instance = self.instances[symbol].clone();
        let definition = self.block("procedures_definition", None, false);
        self.top_level(&definition, position);
        let prototype = self.block("procedures_prototype", Some(&definition), true);
        self.blocks[&definition]["inputs"]["custom_block"] = json!([1, prototype]);
        let mut mutation = instance.mutation();
        let names: Vec<&str> = procedure
            .source
            .template
            .slots()
            .map(|s| s.name.as_str())
            .collect();
        let defaults: Vec<Value> = procedure
            .source
            .template
            .slots()
            .map(|s| {
                if s.arg_type == ArgType::Boolean {
                    json!(false)
                } else {
                    json!("")
                }
            })
            .collect();
        mutation["argumentnames"] = json!(serde_json::to_string(&names)?);
        mutation["argumentdefaults"] = json!(serde_json::to_string(&defaults)?);
        self.blocks[&prototype]["mutation"] = mutation;
        for (slot, argument_id) in procedure
            .source
            .template
            .slots()
            .zip(&instance.argument_ids)
        {
            let argument = self.block(
                if slot.arg_type == ArgType::Boolean {
                    "argument_reporter_boolean"
                } else {
                    "argument_reporter_string_number"
                },
                Some(&prototype),
                true,
            );
            self.blocks[&argument]["fields"]["VALUE"] = json!([slot.name, null]);
            self.blocks[&prototype]["inputs"][argument_id] = json!([1, argument]);
        }
        let first = self.stack(&procedure.body, Some(&definition))?;
        self.blocks[&definition]["next"] = json!(first);
        Ok(())
    }
}

fn scalar_text(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Number(n) => Some(n.to_string()),
        Expr::String(s) | Expr::Menu(s) => Some(s.clone()),
        _ => None,
    }
}

fn json_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn scalar_primitive(expression: &Expr) -> Option<Value> {
    match expression {
        Expr::Number(n) => Some(json!([4, n.to_string()])),
        Expr::String(s) | Expr::Menu(s) => Some(json!([10, s])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{input, procedure_template, tokens_from_text};

    fn span() -> Span {
        Span {
            file: "test.sbdsl".into(),
            line: 7,
            column: 3,
            ..Span::default()
        }
    }
    fn command(source: &str) -> Statement {
        Statement::Command {
            source: source.into(),
            span: span(),
        }
    }
    fn script(body: Vec<Statement>) -> (String, Vec<Statement>, Span) {
        ("start".into(), body, span())
    }
    fn procedure(header: &str, id: &str, body: Vec<Statement>) -> Procedure {
        Procedure {
            template: procedure_template(header, id).unwrap(),
            body,
            span: span(),
        }
    }
    fn blocks(project: &GeneratedProject, target: usize) -> &Map<String, Value> {
        project.json["targets"][target]["blocks"]
            .as_object()
            .unwrap()
    }
    fn opcode<'a>(
        project: &'a GeneratedProject,
        target: usize,
        op: &str,
    ) -> Vec<(&'a str, &'a Value)> {
        blocks(project, target)
            .iter()
            .filter(|(_, b)| b["opcode"] == op)
            .map(|(id, b)| (id.as_str(), b))
            .collect()
    }
    fn test_registry_template(registry: &mut Registry, opcode: &str, text: &str, slot: Slot) {
        registry.templates.push(Template {
            opcode: opcode.into(),
            shape: Shape::Command,
            branch_count: 0,
            namespace: None,
            extension_id: None,
            local_opcode: None,
            tokens: tokens_from_text(text, &[slot]).unwrap(),
            sprite_only: false,
            stage_only: false,
            procedure: None,
            warp: false,
        });
    }
    fn assert_graph(project: &GeneratedProject, target: usize) {
        let blocks = blocks(project, target);
        let mut seen = BTreeSet::new();
        let mut pending: Vec<(&str, Option<&str>)> = blocks
            .iter()
            .filter(|(_, b)| b["topLevel"] == true)
            .map(|(id, _)| (id.as_str(), None))
            .collect();
        while let Some((id, parent)) = pending.pop() {
            assert!(
                seen.insert(id),
                "block has multiple owners or a cycle: {id}"
            );
            let block = &blocks[id];
            assert_eq!(block["parent"], json!(parent), "incorrect parent for {id}");
            if let Some(next) = block["next"].as_str() {
                pending.push((next, Some(id)));
            }
            for input in block["inputs"].as_object().unwrap().values() {
                for descriptor in input.as_array().unwrap().iter().skip(1) {
                    if let Some(child) = descriptor.as_str() {
                        pending.push((child, Some(id)));
                    }
                }
            }
        }
        assert_eq!(seen.len(), blocks.len(), "orphan blocks");
    }

    #[test]
    fn pieces_emit_loose_top_level_stacks_without_a_hat() {
        let mut project = Project::default();
        project
            .stage
            .pieces
            .push((vec![command("等待 (1) 秒")], span()));
        let output = generate(&project, &Registry::new()).unwrap();
        let wait = opcode(&output, 0, "control_wait")[0].1;
        assert_eq!(wait["topLevel"], true);
        assert!(wait["parent"].is_null());
        assert!(wait["next"].is_null());
        assert_graph(&output, 0);
    }

    #[test]
    fn creates_complete_control_graph_and_implicit_globals() {
        let mut project = Project::default();
        project.sprites.push(Target {
            name: "玩家".into(),
            scripts: vec![script(vec![
                command("score = (1 + 2)"),
                Statement::Control {
                    header: "if <>".into(),
                    body: vec![command("说 (<false>)")],
                    else_body: Some(vec![command("等待 (1) 秒")]),
                    span: span(),
                },
                command("移动 (10) 步"),
            ])],
            ..Target::default()
        });
        let output = generate(&project, &Registry::new()).unwrap();
        assert_eq!(
            output.json["targets"][0]["variables"][symbol_id(0, "score", false)],
            json!(["score", 0])
        );
        let condition = opcode(&output, 1, "control_if_else")[0].1;
        assert!(condition["inputs"].get("CONDITION").is_none());
        assert!(condition["inputs"]["SUBSTACK"][1].is_string());
        assert!(condition["inputs"]["SUBSTACK2"][1].is_string());
        assert_eq!(
            opcode(&output, 1, "motion_movesteps")[0].1["inputs"]["STEPS"],
            json!([1, [4, "10"]])
        );
        assert_graph(&output, 1);
    }

    #[test]
    fn public_recursion_preserves_global_binding_and_private_shadowing() {
        let mut project = Project::default();
        project.stage.variables.insert("score".into(), json!(1));
        project.public_procedures = vec![
            procedure(
                "甲",
                "global-a",
                vec![command("score = score"), command("乙")],
            ),
            procedure("乙", "global-b", vec![command("甲")]),
        ];
        let mut sprite = Target {
            name: "玩家".into(),
            ..Target::default()
        };
        sprite.variables.insert("score".into(), json!(2));
        sprite
            .procedures
            .push(procedure("甲", "private-a", vec![command("score = score")]));
        sprite
            .scripts
            .push(script(vec![command("甲"), command("乙")]));
        project.sprites.push(sprite);
        let output = generate(&project, &Registry::new()).unwrap();
        assert!(opcode(&output, 0, "procedures_prototype").is_empty());
        let prototypes = opcode(&output, 1, "procedures_prototype");
        assert_eq!(prototypes.len(), 3);
        let codes: BTreeSet<_> = prototypes
            .iter()
            .map(|(_, b)| b["mutation"]["proccode"].as_str().unwrap())
            .collect();
        assert_eq!(codes.len(), 3);
        let setters = opcode(&output, 1, "data_setvariableto");
        let variable_ids: BTreeSet<_> = setters
            .iter()
            .map(|(_, b)| b["fields"]["VARIABLE"][1].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            variable_ids,
            BTreeSet::from([symbol_id(0, "score", false), symbol_id(1, "score", false)])
        );
        for (_, setter) in setters {
            assert_eq!(
                setter["fields"]["VARIABLE"][1],
                setter["inputs"]["VALUE"][1][2]
            );
        }
        let sources = output.json["meta"]["scratchDSL"]["procedures"]
            .as_array()
            .unwrap();
        assert_eq!(
            sources
                .iter()
                .find(|v| v["symbolId"] == "global-b")
                .unwrap()["calls"],
            json!(["global-a"])
        );
        assert_graph(&output, 1);
    }

    #[test]
    fn procedure_prototype_arguments_warp_and_literal_percent_are_preserved() {
        let mut project = Project::default();
        project.public_procedures.push(procedure(
            "文字%s 值 @x 判断 <@yes> @warp",
            "p",
            vec![
                command("value = @x"),
                Statement::Control {
                    header: "if <@yes>".into(),
                    body: vec![],
                    else_body: None,
                    span: span(),
                },
            ],
        ));
        project
            .stage
            .scripts
            .push(script(vec![command("文字%s 值 (8) 判断 <true>")]));
        let output = generate(&project, &Registry::new()).unwrap();
        let prototype = opcode(&output, 0, "procedures_prototype")[0].1;
        assert_eq!(prototype["shadow"], true);
        assert_eq!(prototype["mutation"]["warp"], "true");
        let proccode = prototype["mutation"]["proccode"].as_str().unwrap();
        assert_eq!(proccode.matches("%s").count(), 1);
        assert_eq!(proccode.matches("%b").count(), 1);
        let names: Value =
            serde_json::from_str(prototype["mutation"]["argumentnames"].as_str().unwrap()).unwrap();
        assert_eq!(names, json!(["x", "yes"]));
        let argument_ids: Vec<String> =
            serde_json::from_str(prototype["mutation"]["argumentids"].as_str().unwrap()).unwrap();
        let call = opcode(&output, 0, "procedures_call")[0].1;
        assert_eq!(
            call["mutation"]["proccode"],
            prototype["mutation"]["proccode"]
        );
        for id in argument_ids {
            assert!(call["inputs"].get(&id).is_some());
            let child = prototype["inputs"][&id][1].as_str().unwrap();
            assert_eq!(blocks(&output, 0)[child]["shadow"], true);
        }
        assert_graph(&output, 0);
    }

    #[test]
    fn validates_unused_procedures_and_restricts_targets_only_when_instantiated() {
        let mut project = Project::default();
        project
            .public_procedures
            .push(procedure("移动动作", "p", vec![command("移动 (1) 步")]));
        assert!(generate(&project, &Registry::new()).is_ok());
        project
            .stage
            .scripts
            .push(script(vec![command("移动动作")]));
        let error = generate(&project, &Registry::new())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("舞台不支持") && error.contains("test.sbdsl:7:3"),
            "{error}"
        );
        project.stage.scripts.clear();
        project.public_procedures[0].body = vec![command("将 (1) 加入 (missing)")];
        assert!(
            generate(&project, &Registry::new())
                .unwrap_err()
                .to_string()
                .contains("未声明")
        );
        project.public_procedures[0].body = vec![command("x = @missing")];
        assert!(
            generate(&project, &Registry::new())
                .unwrap_err()
                .to_string()
                .contains("参数")
        );
        project.public_procedures[0].body = vec![command("完全未知的积木")];
        assert!(generate(&project, &Registry::new()).is_err());
    }

    #[test]
    fn encodes_menu_fields_selected_shadows_and_scalar_overlay() {
        let mut registry = Registry::new();
        let mut field = input("MODE", ArgType::String);
        field.storage = Storage::Field;
        field.shadow = None;
        field.menu = Some(Menu {
            items: Some(vec![("快速".into(), "fast".into())]),
            accept_reporters: false,
        });
        test_registry_template(&mut registry, "custom_field", "字段 [MODE]", field);
        let mut menu = input("MODE", ArgType::String);
        menu.shadow = Some(Shadow {
            opcode: "custom_menu_modes".into(),
            field: "modes".into(),
            primitive: None,
        });
        menu.default = json!("fast");
        menu.menu = Some(Menu {
            items: Some(vec![("快速".into(), "fast".into())]),
            accept_reporters: true,
        });
        test_registry_template(&mut registry, "custom_menu", "输入 [MODE]", menu.clone());
        menu.default = Value::Null;
        menu.menu.as_mut().unwrap().items = None;
        test_registry_template(&mut registry, "custom_dynamic", "动态 [MODE]", menu);
        let mut project = Project::default();
        project.stage.scripts.push(script(vec![
            command("字段 '快速'"),
            command("输入 ('fast')"),
            command("输入 (\"arbitrary\")"),
            command("动态 (42)"),
        ]));
        let output = generate(&project, &registry).unwrap();
        assert_eq!(
            opcode(&output, 0, "custom_field")[0].1["fields"]["MODE"],
            json!(["fast", null])
        );
        let menus = opcode(&output, 0, "custom_menu");
        assert_eq!(menus[0].1["inputs"]["MODE"][0], 1);
        let selected = menus[0].1["inputs"]["MODE"][1].as_str().unwrap();
        assert_eq!(
            blocks(&output, 0)[selected]["fields"]["modes"],
            json!(["fast", null])
        );
        assert_eq!(menus[1].1["inputs"]["MODE"][0], 3);
        assert_eq!(menus[1].1["inputs"]["MODE"][1], json!([10, "arbitrary"]));
        assert_eq!(
            opcode(&output, 0, "custom_dynamic")[0].1["inputs"]["MODE"],
            json!([2, [4, "42"]])
        );
        assert_graph(&output, 0);
    }

    #[test]
    fn slot_types_select_angle_color_and_text_primitive_tags() {
        let mut registry = Registry::new();
        test_registry_template(
            &mut registry,
            "custom_color",
            "颜色 [COLOR]",
            input("COLOR", ArgType::Color),
        );
        let mut project = Project::default();
        project.sprites.push(Target {
            name: "玩家".into(),
            scripts: vec![script(vec![
                command("面向 (45) 方向"),
                command("颜色 (\"#AABBCC\")"),
                command("说 (12)"),
            ])],
            ..Target::default()
        });
        let output = generate(&project, &registry).unwrap();
        assert_eq!(
            opcode(&output, 1, "motion_pointindirection")[0].1["inputs"]["DIRECTION"],
            json!([1, [8, "45"]])
        );
        assert_eq!(
            opcode(&output, 1, "custom_color")[0].1["inputs"]["COLOR"],
            json!([1, [9, "#AABBCC"]])
        );
        assert_eq!(
            opcode(&output, 1, "looks_say")[0].1["inputs"]["MESSAGE"],
            json!([1, [10, "12"]])
        );
    }

    #[test]
    fn broadcast_fields_and_inputs_share_stage_identity() {
        let mut project = Project::default();
        project
            .stage
            .scripts
            .push(script(vec![command("广播 (\"消息\")")]));
        project.sprites.push(Target {
            name: "玩家".into(),
            scripts: vec![("当收到广播 \"消息\"".into(), vec![], span())],
            ..Target::default()
        });
        let output = generate(&project, &Registry::new()).unwrap();
        let receive = opcode(&output, 1, "event_whenbroadcastreceived")[0].1;
        let field = &receive["fields"]["BROADCAST_OPTION"];
        let send = opcode(&output, 0, "event_broadcast")[0].1;
        assert_eq!(
            send["inputs"]["BROADCAST_INPUT"],
            json!([1, [11, "消息", field[1]]])
        );
        assert_eq!(
            output.json["targets"][0]["broadcasts"][field[1].as_str().unwrap()],
            "消息"
        );
    }
}
