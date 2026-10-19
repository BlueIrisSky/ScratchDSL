//! Binding shared by builds and live diagnostics. This module emits no blocks or assets.
use crate::{Error, Result, model::*, registry::Registry};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

mod diagnostics;
pub(crate) use diagnostics::diagnose_project;

pub(crate) struct BoundProject {
    pub(crate) targets: Vec<Target>,
    pub(crate) procedures: BTreeMap<String, BoundProcedure>,
    pub(crate) scripts: Vec<Vec<BoundScript>>,
    pub(crate) selected: Vec<BTreeSet<String>>,
    pub(crate) procedure_order: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct BoundStatement {
    pub(crate) invocation: Invocation,
    pub(crate) body: Option<Vec<BoundStatement>>,
    pub(crate) otherwise: Option<Vec<BoundStatement>>,
    pub(crate) span: Span,
}

pub(crate) struct BoundProcedure {
    pub(crate) source: Procedure,
    /// None is the public, lexically global scope.
    pub(crate) scope: Option<usize>,
    pub(crate) body: Vec<BoundStatement>,
}

pub(crate) struct BoundScript {
    pub(crate) hat: Invocation,
    pub(crate) body: Vec<BoundStatement>,
    pub(crate) span: Span,
}

#[derive(Clone)]
struct Context {
    scope: Option<usize>,
    visible: Vec<Template>,
    parameters: BTreeMap<String, bool>,
}

struct Binder<'a> {
    registry: &'a Registry,
    targets: Vec<Target>,
    public: Vec<Procedure>,
}

impl Binder<'_> {
    fn context(&self, scope: Option<usize>, procedure: Option<&Procedure>) -> Context {
        let mut visible: Vec<Template> = self.public.iter().map(|p| p.template.clone()).collect();
        if let Some(index) = scope {
            let private = &self.targets[index].procedures;
            let signatures: BTreeSet<String> =
                private.iter().map(|p| p.template.signature()).collect();
            visible.retain(|p| !signatures.contains(&p.signature()));
            visible.extend(private.iter().map(|p| p.template.clone()));
        }
        Context {
            scope,
            visible,
            parameters: procedure
                .map(|p| {
                    p.template
                        .slots()
                        .map(|s| (s.name.clone(), s.arg_type == ArgType::Boolean))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn invocation(
        &mut self,
        source: &str,
        shape: Shape,
        context: &Context,
        span: &Span,
    ) -> Result<Invocation> {
        let mut invocation = self
            .registry
            .parse(source, shape, &context.visible)
            .map_err(|e| e.at(span))?;
        self.bind_invocation(&mut invocation, context)
            .map_err(|e| e.at(span))?;
        Ok(invocation)
    }

    fn bind_invocation(&mut self, invocation: &mut Invocation, context: &Context) -> Result<()> {
        if invocation.template.slots().count() != invocation.args.len() {
            return Err(Error::new("积木参数数量与模板不一致"));
        }
        for expression in &mut invocation.args {
            self.bind_expression(expression, context)?;
        }
        Ok(())
    }

    fn bind_expression(&mut self, expression: &mut Expr, context: &Context) -> Result<()> {
        match expression {
            Expr::Symbol { name, list, global } => {
                let local = if !*global {
                    context.scope.filter(|i| {
                        if *list {
                            self.targets[*i].lists.contains_key(name)
                        } else {
                            self.targets[*i].variables.contains_key(name)
                        }
                    })
                } else {
                    None
                };
                if let Some(index) = local {
                    *global = index == 0;
                } else {
                    *global = true;
                    if *list {
                        if !self.targets[0].lists.contains_key(name) {
                            return Err(Error::new(format!("列表 {name:?} 未声明")));
                        }
                    } else {
                        self.targets[0]
                            .variables
                            .entry(name.clone())
                            .or_insert(json!(0));
                    }
                }
            }
            Expr::Param { name, boolean } => match context.parameters.get(name) {
                Some(expected) if expected == boolean => {}
                Some(_) => {
                    return Err(Error::new(format!("参数 {name:?} 的布尔/普通引用类型不符")));
                }
                None => return Err(Error::new(format!("参数 {name:?} 未在当前自制积木中声明"))),
            },
            Expr::Call(invocation) => self.bind_invocation(invocation, context)?,
            _ => {}
        }
        Ok(())
    }

    fn statements(
        &mut self,
        statements: &[Statement],
        context: &Context,
    ) -> Result<Vec<BoundStatement>> {
        statements
            .iter()
            .map(|statement| match statement {
                Statement::Command { source, span } => Ok(BoundStatement {
                    invocation: self.invocation(source, Shape::Command, context, span)?,
                    body: None,
                    otherwise: None,
                    span: span.clone(),
                }),
                Statement::Control {
                    header,
                    body,
                    else_body,
                    span,
                } => {
                    let mut invocation = self.invocation(header, Shape::Control, context, span)?;
                    if else_body.is_some() {
                        if invocation.template.opcode != "control_if"
                            && invocation.template.opcode != "control_if_else"
                        {
                            return Err(Error::new("只有如果控制积木可以包含否则分支").at(span));
                        }
                        invocation.template.opcode = "control_if_else".into();
                    }
                    Ok(BoundStatement {
                        invocation,
                        body: Some(self.statements(body, context)?),
                        otherwise: else_body
                            .as_ref()
                            .map(|s| self.statements(s, context))
                            .transpose()?,
                        span: span.clone(),
                    })
                }
            })
            .collect()
    }
}

pub(crate) fn bind_project(project: &Project, registry: &Registry) -> Result<BoundProject> {
    let mut targets = vec![project.stage.clone()];
    targets.extend(project.sprites.iter().cloned());
    let mut binder = Binder {
        registry,
        targets,
        public: project.public_procedures.clone(),
    };
    let mut procedures = BTreeMap::<String, BoundProcedure>::new();
    let mut procedure_order = Vec::new();
    let scopes: Vec<(Option<usize>, Vec<Procedure>)> =
        std::iter::once((None, binder.public.clone()))
            .chain(
                binder
                    .targets
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (Some(i), t.procedures.clone())),
            )
            .collect();
    for (scope, definitions) in scopes {
        let mut signatures = BTreeSet::new();
        for definition in definitions {
            if !signatures.insert(definition.template.signature()) {
                return Err(Error::new("同一作用域存在重复自制积木签名").at(&definition.span));
            }
            let id = definition
                .template
                .procedure
                .clone()
                .ok_or_else(|| Error::new("自制积木缺少源符号 ID").at(&definition.span))?;
            if procedures.contains_key(&id) {
                return Err(
                    Error::new(format!("重复自制积木源符号 ID {id:?}")).at(&definition.span)
                );
            }
            let mut names = BTreeSet::new();
            if definition
                .template
                .slots()
                .any(|s| !names.insert(s.name.clone()))
            {
                return Err(Error::new("自制积木参数名称重复").at(&definition.span));
            }
            let context = binder.context(scope, Some(&definition));
            let body = binder.statements(&definition.body, &context)?;
            procedure_order.push(id.clone());
            procedures.insert(
                id,
                BoundProcedure {
                    source: definition,
                    scope,
                    body,
                },
            );
        }
    }
    let mut scripts = Vec::new();
    for index in 0..binder.targets.len() {
        let context = binder.context(Some(index), None);
        let mut target_scripts = Vec::new();
        for (header, body, span) in binder.targets[index].scripts.clone() {
            target_scripts.push(BoundScript {
                hat: binder.invocation(&header, Shape::Hat, &context, &span)?,
                body: binder.statements(&body, &context)?,
                span,
            });
        }
        scripts.push(target_scripts);
    }

    let mut selected_targets = Vec::new();
    for (index, target_scripts) in scripts.iter().enumerate() {
        let mut selected: BTreeSet<String> = procedures
            .iter()
            .filter_map(|(id, p)| (p.scope == Some(index)).then_some(id.clone()))
            .collect();
        for script in target_scripts {
            collect_invocation_calls(&script.hat, &mut selected);
            selected.extend(called_procedures(&script.body));
        }
        let mut pending: Vec<String> = selected.iter().cloned().collect();
        while let Some(id) = pending.pop() {
            let procedure = procedures
                .get(&id)
                .ok_or_else(|| Error::new(format!("找不到绑定的过程 {id:?}")))?;
            if procedure.scope.is_some() && procedure.scope != Some(index) {
                return Err(Error::new("过程调用越过角色私有作用域").at(&procedure.source.span));
            }
            for callee in called_procedures(&procedure.body) {
                if selected.insert(callee.clone()) {
                    pending.push(callee);
                }
            }
        }

        for script in target_scripts {
            validate_invocation_target(&script.hat, index, &script.span)?;
            validate_body_target(&script.body, index)?;
        }
        for id in &selected {
            validate_body_target(&procedures[id].body, index)?;
        }
        selected_targets.push(selected);
    }
    Ok(BoundProject {
        targets: binder.targets,
        procedures,
        scripts,
        selected: selected_targets,
        procedure_order,
    })
}

fn validate_invocation_target(invocation: &Invocation, target: usize, span: &Span) -> Result<()> {
    validate_template_target(&invocation.template, target, span)?;
    for argument in &invocation.args {
        if let Expr::Call(call) = argument {
            validate_invocation_target(call, target, span)?;
        }
    }
    Ok(())
}

fn validate_template_target(template: &Template, target: usize, span: &Span) -> Result<()> {
    if target == 0 && template.sprite_only {
        return Err(Error::new(format!("舞台不支持积木 {}", template.opcode)).at(span));
    }
    if target != 0 && template.stage_only {
        return Err(Error::new(format!("角色不支持仅舞台可用的积木 {}", template.opcode)).at(span));
    }
    Ok(())
}

fn validate_body_target(body: &[BoundStatement], target: usize) -> Result<()> {
    for statement in body {
        validate_invocation_target(&statement.invocation, target, &statement.span)?;
        if let Some(body) = &statement.body {
            validate_body_target(body, target)?;
        }
        if let Some(body) = &statement.otherwise {
            validate_body_target(body, target)?;
        }
    }
    Ok(())
}

fn collect_invocation_calls(invocation: &Invocation, output: &mut BTreeSet<String>) {
    if let Some(id) = &invocation.template.procedure {
        output.insert(id.clone());
    }
    for arg in &invocation.args {
        if let Expr::Call(call) = arg {
            collect_invocation_calls(call, output);
        }
    }
}

pub(crate) fn called_procedures(body: &[BoundStatement]) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for statement in body {
        collect_invocation_calls(&statement.invocation, &mut result);
        if let Some(body) = &statement.body {
            result.extend(called_procedures(body));
        }
        if let Some(body) = &statement.otherwise {
            result.extend(called_procedures(body));
        }
    }
    result
}
