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
    pub(crate) branches: Vec<Vec<BoundStatement>>,
    pub(crate) span: Span,
}

pub(crate) struct BoundProcedure {
    pub(crate) source: Procedure,
    /// None is the public, lexically global scope.
    pub(crate) scope: Option<usize>,
    pub(crate) body: Vec<BoundStatement>,
}

pub(crate) struct BoundScript {
    /// None for a piece: a loose top-level stack without a hat.
    pub(crate) hat: Option<Invocation>,
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
    fn data_target(&self, name: &str, global: bool, scope: Option<usize>) -> usize {
        if global {
            0
        } else {
            scope
                .filter(|index| {
                    self.targets[*index].variables.contains_key(name)
                        || self.targets[*index].lists.contains_key(name)
                })
                .unwrap_or(0)
        }
    }

    fn infer_target(
        &mut self,
        name: &str,
        global: bool,
        list: bool,
        scope: Option<usize>,
    ) -> Result<()> {
        let target = self.data_target(name, global, scope);
        let opposite = if list {
            &self.targets[target].variables
        } else {
            &self.targets[target].lists
        };
        if opposite.contains_key(name) {
            return Err(Error::new(format!(
                "名称 {name:?} 已确定为{}，不能改为{}",
                if list { "变量" } else { "列表" },
                if list { "列表" } else { "变量" },
            )));
        }
        if target == 0
            && self.targets.iter().skip(1).any(|target| {
                if list {
                    target.variables.contains_key(name)
                } else {
                    target.lists.contains_key(name)
                }
            })
        {
            return Err(Error::new(format!(
                "隐式全局{} {name:?} 与角色中的同名{}冲突",
                if list { "列表" } else { "变量" },
                if list { "变量" } else { "列表" },
            )));
        }
        if list {
            self.targets[target]
                .lists
                .entry(name.into())
                .or_insert(json!([]));
        } else {
            self.targets[target]
                .variables
                .entry(name.into())
                .or_insert(json!(0));
        }
        Ok(())
    }

    fn infer_statement_types(
        &mut self,
        body: &[Statement],
        scope: Option<usize>,
        errors: &mut Vec<Error>,
    ) {
        for statement in body {
            match statement {
                Statement::Command { source, span } => {
                    if let Some(assignment) = crate::parser::assignment_parts(source)
                        && let Ok((name, global)) =
                            crate::parser::parse_reference(assignment.target)
                        && let Err(error) = self.infer_target(&name, global, false, scope)
                    {
                        errors.push(error.at(span));
                    }
                }
                Statement::ListAssignment {
                    name, global, span, ..
                } => {
                    if let Err(error) = self.infer_target(name, *global, true, scope) {
                        errors.push(error.at(span));
                    }
                }
                Statement::Control {
                    body, else_body, ..
                } => {
                    self.infer_statement_types(body, scope, errors);
                    if let Some(body) = else_body {
                        self.infer_statement_types(body, scope, errors);
                    }
                }
                Statement::ExtensionControl { branches, .. } => {
                    for branch in branches {
                        self.infer_statement_types(branch, scope, errors);
                    }
                }
            }
        }
    }

    fn infer_assignment_types(&mut self, project: &Project) -> Vec<Error> {
        let mut errors = Vec::new();
        for definition in &project.public_procedures {
            self.infer_statement_types(&definition.body, None, &mut errors);
        }
        for (index, target) in std::iter::once(&project.stage)
            .chain(&project.sprites)
            .enumerate()
        {
            for definition in &target.procedures {
                self.infer_statement_types(&definition.body, Some(index), &mut errors);
            }
            for (_, body, _) in &target.scripts {
                self.infer_statement_types(body, Some(index), &mut errors);
            }
            for (body, _) in &target.pieces {
                self.infer_statement_types(body, Some(index), &mut errors);
            }
        }
        errors
    }

    fn list_assignment(
        &mut self,
        name: &str,
        global: bool,
        values: &[serde_json::Value],
        context: &Context,
        span: &Span,
    ) -> Result<Vec<BoundStatement>> {
        self.infer_target(name, global, true, context.scope)
            .map_err(|error| error.at(span))?;
        let list = Expr::Symbol {
            name: name.into(),
            list: true,
            global,
        };
        let mut statements = Vec::with_capacity(values.len() + 1);
        let mut emit = |opcode: &str, value: Option<Expr>| -> Result<()> {
            let template = self
                .registry
                .builtin_template(opcode)
                .map_err(|error| error.at(span))?;
            let args = template
                .slots()
                .map(|slot| {
                    if slot.arg_type == ArgType::List {
                        list.clone()
                    } else {
                        value.clone().expect("list append has an item")
                    }
                })
                .collect();
            let mut invocation = Invocation { template, args };
            self.bind_invocation(&mut invocation, context)
                .map_err(|error| error.at(span))?;
            statements.push(BoundStatement {
                invocation,
                branches: Vec::new(),
                span: span.clone(),
            });
            Ok(())
        };
        emit("data_deletealloflist", None)?;
        for value in values {
            let value = match value {
                serde_json::Value::String(value) => Expr::String(value.clone()),
                serde_json::Value::Number(value) => Expr::Number(
                    value
                        .as_f64()
                        .ok_or_else(|| Error::new("列表数值超出范围").at(span))?,
                ),
                serde_json::Value::Bool(value) => Expr::Bool(*value),
                _ => return Err(Error::new("列表赋值只接受标量字面量").at(span)),
            };
            emit("data_addtolist", Some(value))?;
        }
        Ok(statements)
    }

    fn data_types(&self, context: &Context) -> BTreeMap<(String, bool), bool> {
        let mut types = BTreeMap::new();
        for (list, names) in [
            (false, &self.targets[0].variables),
            (true, &self.targets[0].lists),
        ] {
            for name in names.keys() {
                types.insert((name.clone(), false), list);
                types.insert((name.clone(), true), list);
            }
        }
        if let Some(index) = context.scope {
            for (list, names) in [
                (false, &self.targets[index].variables),
                (true, &self.targets[index].lists),
            ] {
                for name in names.keys() {
                    types.insert((name.clone(), false), list);
                }
            }
        }
        types
    }

    fn parse_invocation(
        &self,
        source: &str,
        shape: Shape,
        context: &Context,
    ) -> Result<Invocation> {
        self.registry
            .parse_typed(source, shape, &context.visible, &self.data_types(context))
    }

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
            .parse_invocation(source, shape, context)
            .map_err(|e| e.at(span))?;
        self.bind_invocation(&mut invocation, context)
            .map_err(|e| e.at(span))?;
        Ok(invocation)
    }

    fn bind_invocation(&mut self, invocation: &mut Invocation, context: &Context) -> Result<()> {
        if invocation.template.slots().count() != invocation.args.len() {
            return Err(Error::new("积木参数数量与模板不一致"));
        }
        for (slot, expression) in invocation.template.slots().zip(&mut invocation.args) {
            self.bind_expression(expression, context, Some(slot.arg_type))?;
            validate_bound_data_slot(slot, expression)?;
        }
        Ok(())
    }

    fn bind_expression(
        &mut self,
        expression: &mut Expr,
        context: &Context,
        expected: Option<ArgType>,
    ) -> Result<()> {
        match expression {
            Expr::Symbol { name, list, global } => {
                let index = self.data_target(name, *global, context.scope);
                *global = index == 0;
                *list = self.targets[index].lists.contains_key(name);
                if *global && !*list && !self.targets[0].variables.contains_key(name) {
                    if expected == Some(ArgType::List) {
                        return Err(Error::new(format!("列表 {name:?} 未声明")));
                    }
                    if self
                        .targets
                        .iter()
                        .any(|target| target.lists.contains_key(name))
                    {
                        return Err(Error::new(format!(
                            "隐式全局变量 {name:?} 与角色中的同名列表冲突，变量和列表不能互相转换"
                        )));
                    }
                    self.targets[0]
                        .variables
                        .entry(name.clone())
                        .or_insert(json!(0));
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
        let mut bound = Vec::with_capacity(statements.len());
        for statement in statements {
            let statement = match statement {
                Statement::ListAssignment {
                    name,
                    global,
                    values,
                    span,
                } => {
                    bound.extend(self.list_assignment(name, *global, values, context, span)?);
                    continue;
                }
                Statement::Command { source, span } => BoundStatement {
                    invocation: self.invocation(source, Shape::Command, context, span)?,
                    branches: Vec::new(),
                    span: span.clone(),
                },
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
                        invocation.template.branch_count = 2;
                    }
                    let mut branches = vec![self.statements(body, context)?];
                    if let Some(otherwise) = else_body {
                        branches.push(self.statements(otherwise, context)?);
                    }
                    BoundStatement {
                        invocation,
                        branches,
                        span: span.clone(),
                    }
                }
                Statement::ExtensionControl {
                    header,
                    branches,
                    span,
                } => {
                    let invocation = self.invocation(header, Shape::Control, context, span)?;
                    validate_extension_branches(&invocation.template, branches.len())
                        .map_err(|error| error.at(span))?;
                    BoundStatement {
                        invocation,
                        branches: branches
                            .iter()
                            .map(|branch| self.statements(branch, context))
                            .collect::<Result<_>>()?,
                        span: span.clone(),
                    }
                }
            };
            bound.push(statement);
        }
        Ok(bound)
    }
}

fn validate_extension_branches(template: &Template, count: usize) -> Result<()> {
    if template.extension_id.is_none() || template.branch_count == 0 {
        return Err(Error::new("只有扩展 C 型积木可以使用这种代码体"));
    }
    if count != template.branch_count {
        return Err(Error::new(format!(
            "扩展积木 {} 需要 {} 个分支代码体，实际提供 {} 个；后续分支使用同级 else:",
            template.opcode, template.branch_count, count
        )));
    }
    Ok(())
}

fn validate_bound_data_slot(slot: &Slot, expression: &Expr) -> Result<()> {
    if let Expr::Symbol { name, list, .. } = expression {
        if *list && slot.arg_type != ArgType::List {
            return Err(Error::new(format!(
                "列表 {name:?} 不能用于 {:?} 槽位，列表不能转换为变量或普通值",
                slot.arg_type
            )));
        }
        if !*list && slot.arg_type == ArgType::List {
            return Err(Error::new(format!(
                "变量 {name:?} 不能用于列表槽位，变量不能转换为列表"
            )));
        }
    }
    Ok(())
}

fn validate_project_data_names(targets: &[Target]) -> Result<()> {
    for target in targets {
        for name in target.variables.keys() {
            if target.lists.contains_key(name) || targets[0].lists.contains_key(name) {
                return Err(Error::new(format!(
                    "名称 {name:?} 不能同时声明为变量和列表"
                )));
            }
        }
        for name in target.lists.keys() {
            if targets[0].variables.contains_key(name) {
                return Err(Error::new(format!(
                    "名称 {name:?} 不能同时声明为变量和列表"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn bind_project(project: &Project, registry: &Registry) -> Result<BoundProject> {
    let mut targets = vec![project.stage.clone()];
    targets.extend(project.sprites.iter().cloned());
    validate_project_data_names(&targets)?;
    let mut binder = Binder {
        registry,
        targets,
        public: project.public_procedures.clone(),
    };
    if let Some(error) = binder.infer_assignment_types(project).into_iter().next() {
        return Err(error);
    }
    let mut procedures = BTreeMap::<String, BoundProcedure>::new();
    let mut procedure_order = Vec::new();
    for (scope, definitions) in std::iter::once((None, project.public_procedures.as_slice())).chain(
        std::iter::once(&project.stage)
            .chain(&project.sprites)
            .enumerate()
            .map(|(i, t)| (Some(i), t.procedures.as_slice())),
    ) {
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
            let context = binder.context(scope, Some(definition));
            let body = binder.statements(&definition.body, &context)?;
            procedure_order.push(id.clone());
            procedures.insert(
                id,
                BoundProcedure {
                    source: definition.clone(),
                    scope,
                    body,
                },
            );
        }
    }
    let mut scripts = Vec::new();
    for (index, target) in std::iter::once(&project.stage)
        .chain(&project.sprites)
        .enumerate()
    {
        let context = binder.context(Some(index), None);
        let mut target_scripts = Vec::new();
        for (header, body, span) in &target.scripts {
            target_scripts.push(BoundScript {
                hat: Some(binder.invocation(header, Shape::Hat, &context, span)?),
                body: binder.statements(body, &context)?,
                span: span.clone(),
            });
        }
        for (body, span) in &target.pieces {
            target_scripts.push(BoundScript {
                hat: None,
                body: binder.statements(body, &context)?,
                span: span.clone(),
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
            if let Some(hat) = &script.hat {
                collect_invocation_calls(hat, &mut selected);
            }
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
            if let Some(hat) = &script.hat {
                validate_invocation_target(hat, index, &script.span)?;
            }
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
        for branch in &statement.branches {
            validate_body_target(branch, target)?;
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
        for branch in &statement.branches {
            result.extend(called_procedures(branch));
        }
    }
    result
}
