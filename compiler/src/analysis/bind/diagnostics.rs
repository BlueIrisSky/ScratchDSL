//! Recovering semantic analysis of the assembled AST. Invalid calls are omitted
//! from the dependency graph; their independent siblings and bodies still bind.
use super::*;
use std::collections::btree_map::Entry;

struct CheckedCall {
    invocation: Invocation,
    span: Span,
}

struct CheckedProcedure {
    scope: Option<usize>,
    span: Span,
    calls: Vec<CheckedCall>,
}

#[derive(Default)]
struct Diagnostics {
    errors: Vec<Error>,
    seen: BTreeSet<(String, usize, usize, usize, usize, String)>,
}

impl Diagnostics {
    fn push(&mut self, error: Error) {
        let span = error.span().cloned().unwrap_or_default();
        let key = (
            span.file,
            span.start,
            span.end,
            span.line,
            span.column,
            error.0.clone(),
        );
        if self.seen.insert(key) {
            self.errors.push(error);
        }
    }

    fn invocation(
        &mut self,
        binder: &mut Binder<'_>,
        source: &str,
        shape: Shape,
        context: &Context,
        span: &Span,
    ) -> Option<CheckedCall> {
        match binder.registry.parse(source, shape, &context.visible) {
            Ok(mut invocation) => {
                self.bind_invocation(binder, &mut invocation, context, span);
                Some(CheckedCall {
                    invocation,
                    span: span.clone(),
                })
            }
            Err(error) => {
                self.push(error.at(span));
                None
            }
        }
    }

    fn bind_invocation(
        &mut self,
        binder: &mut Binder<'_>,
        invocation: &mut Invocation,
        context: &Context,
        span: &Span,
    ) {
        if invocation.template.slots().count() != invocation.args.len() {
            self.push(Error::new("积木参数数量与模板不一致").at(span));
        }
        for expression in &mut invocation.args {
            if let Expr::Call(call) = expression {
                self.bind_invocation(binder, call, context, span);
            } else if let Err(error) = binder.bind_expression(expression, context) {
                self.push(error.at(span));
            }
        }
    }

    fn statements(
        &mut self,
        binder: &mut Binder<'_>,
        statements: &[Statement],
        context: &Context,
        calls: &mut Vec<CheckedCall>,
    ) {
        for statement in statements {
            match statement {
                Statement::Command { source, span } => {
                    if let Some(call) =
                        self.invocation(binder, source, Shape::Command, context, span)
                    {
                        calls.push(call);
                    }
                }
                Statement::Control {
                    header,
                    body,
                    else_body,
                    span,
                } => {
                    if let Some(call) =
                        self.invocation(binder, header, Shape::Control, context, span)
                    {
                        if else_body.is_some()
                            && call.invocation.template.opcode != "control_if"
                            && call.invocation.template.opcode != "control_if_else"
                        {
                            self.push(Error::new("只有如果控制积木可以包含否则分支").at(span));
                        }
                        calls.push(call);
                    }
                    // A bad condition does not make either branch uncheckable.
                    self.statements(binder, body, context, calls);
                    if let Some(body) = else_body {
                        self.statements(binder, body, context, calls);
                    }
                }
            }
        }
    }

    fn target(&mut self, invocation: &Invocation, target: usize, span: &Span) {
        if let Err(error) = validate_template_target(&invocation.template, target, span) {
            self.push(error);
        }
        for expression in &invocation.args {
            if let Expr::Call(call) = expression {
                self.target(call, target, span);
            }
        }
    }

    fn calls_target(&mut self, calls: &[CheckedCall], target: usize) {
        for call in calls {
            self.target(&call.invocation, target, &call.span);
        }
    }
}

fn called(calls: &[CheckedCall]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for call in calls {
        collect_invocation_calls(&call.invocation, &mut ids);
    }
    ids
}

/// Check all recoverable statements without generating blocks or aborting at
/// the first error. Scope construction, expression binding and target rules are
/// shared with strict compilation; only error collection and traversal differ.
pub(crate) fn diagnose_project(project: &Project, registry: &Registry) -> Vec<Error> {
    let mut targets = vec![project.stage.clone()];
    targets.extend(project.sprites.iter().cloned());
    let mut binder = Binder {
        registry,
        targets,
        public: project.public_procedures.clone(),
    };
    let mut diagnostics = Diagnostics::default();
    let mut procedures = BTreeMap::<String, CheckedProcedure>::new();
    let scopes: Vec<_> = std::iter::once((None, binder.public.clone()))
        .chain(
            binder
                .targets
                .iter()
                .enumerate()
                .map(|(index, target)| (Some(index), target.procedures.clone())),
        )
        .collect();
    for (scope, definitions) in scopes {
        let mut signatures = BTreeSet::new();
        for definition in definitions {
            if !signatures.insert(definition.template.signature()) {
                diagnostics.push(Error::new("同一作用域存在重复自制积木签名").at(&definition.span));
            }
            let mut names = BTreeSet::new();
            if definition
                .template
                .slots()
                .any(|slot| !names.insert(slot.name.clone()))
            {
                diagnostics.push(Error::new("自制积木参数名称重复").at(&definition.span));
            }
            let context = binder.context(scope, Some(&definition));
            let mut calls = Vec::new();
            diagnostics.statements(&mut binder, &definition.body, &context, &mut calls);
            // Private procedures are emitted on their owner even when uncalled.
            // Checking them here also covers malformed/duplicate definition IDs.
            if let Some(index) = scope {
                diagnostics.calls_target(&calls, index);
            }
            let Some(id) = definition.template.procedure else {
                diagnostics.push(Error::new("自制积木缺少源符号 ID").at(&definition.span));
                continue;
            };
            match procedures.entry(id) {
                Entry::Occupied(entry) => diagnostics.push(
                    Error::new(format!("重复自制积木源符号 ID {:?}", entry.key()))
                        .at(&definition.span),
                ),
                Entry::Vacant(entry) => {
                    entry.insert(CheckedProcedure {
                        scope,
                        span: definition.span,
                        calls,
                    });
                }
            }
        }
    }

    for index in 0..binder.targets.len() {
        let context = binder.context(Some(index), None);
        let mut calls = Vec::new();
        for (header, body, span) in binder.targets[index].scripts.clone() {
            if let Some(call) =
                diagnostics.invocation(&mut binder, &header, Shape::Hat, &context, &span)
            {
                calls.push(call);
            }
            diagnostics.statements(&mut binder, &body, &context, &mut calls);
        }
        diagnostics.calls_target(&calls, index);

        let mut pending: Vec<_> = procedures
            .iter()
            .filter_map(|(id, procedure)| (procedure.scope == Some(index)).then_some(id.clone()))
            .chain(called(&calls))
            .collect();
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let Some(procedure) = procedures.get(&id) else {
                diagnostics.push(Error::new(format!("找不到绑定的过程 {id:?}")));
                continue;
            };
            if procedure.scope.is_some() && procedure.scope != Some(index) {
                diagnostics.push(Error::new("过程调用越过角色私有作用域").at(&procedure.span));
                continue;
            }
            diagnostics.calls_target(&procedure.calls, index);
            pending.extend(called(&procedure.calls));
        }
    }
    diagnostics.errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analysis::OverlaySource, project};
    use std::path::Path;

    fn load(source: &str) -> Project {
        let path = Path::new("diagnostics.sbdsl");
        let overlays = BTreeMap::from([(path.to_path_buf(), source.to_owned())]);
        project::load_with_provider(path, Path::new("."), &OverlaySource::new(&overlays)).unwrap()
    }

    fn messages(project: &Project) -> Vec<String> {
        diagnose_project(project, &Registry::new())
            .into_iter()
            .map(|error| error.0)
            .collect()
    }

    #[test]
    fn continues_after_bad_hats_conditions_and_independent_commands() {
        let project = load(
            r#"
未知事件 {
    [第一处未知积木]
    如果 1 那么 {
        [将 1 加入 $list:then_missing]
    } 否则 {
        [将 2 加入 $list:else_missing]
    }
    [将 $value 设为 @outside]
}
当绿旗被点击 {
    [第二处未知积木]
    [移动 1 步]
}
"#,
        );
        let errors = diagnose_project(&project, &Registry::new());
        for expected in [
            "未知事件",
            "第一处未知积木",
            "如果 1 那么",
            "then_missing",
            "else_missing",
            "outside",
            "第二处未知积木",
            "舞台不支持积木 motion_movesteps",
        ] {
            assert!(
                errors.iter().any(|error| error.0.contains(expected)),
                "{expected}: {errors:?}"
            );
        }
        assert_eq!(errors.len(), 8, "{errors:?}");
        assert!(errors.iter().all(|error| error.span().is_some()));
    }

    #[test]
    fn reports_each_bad_reference_and_checks_parameter_types() {
        let project = load(
            r#"
定义自制积木 检查 @n <@ok> {
    [将 $implicit 设为 (@first + @second)]
    如果 <@n> 那么 {}
    [等待 @ok 秒]
    [将 1 加入 $list:missing]
}
"#,
        );
        let errors = messages(&project);
        for expected in ["first", "second", "n", "ok", "missing"] {
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains(&format!("{expected:?}"))),
                "{errors:?}"
            );
        }
        assert_eq!(errors.len(), 5, "{errors:?}");
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains("引用类型不符"))
                .count(),
            2
        );
        assert!(
            project.stage.variables.is_empty(),
            "analysis must not mutate the input AST"
        );
    }

    #[test]
    fn target_errors_survive_reference_errors_in_the_same_call() {
        let project = load("当绿旗被点击 { [移动 @missing 步] }");
        let errors = messages(&project);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("参数 \"missing\""))
        );
        assert!(errors.iter().any(|error| error.contains("舞台不支持")));
    }

    #[test]
    fn recursive_public_calls_are_checked_once_on_each_target() {
        let project = load(
            r#"
定义自制积木 甲 {
    [乙]
    [移动 1 步]
}
定义自制积木 乙 {
    [甲]
    [将 $x 设为 (x 坐标)]
}
当绿旗被点击 {
    [甲]
    [甲]
}
@sprite "玩家" {
    @init {}
    当绿旗被点击 { [乙] }
}
"#,
        );
        let errors = messages(&project);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("motion_movesteps"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("motion_xposition"))
        );
    }

    #[test]
    fn public_lexical_scope_and_private_shadowing_match_strict_binding() {
        let project = load(
            r#"
@list logs = []
定义自制积木 甲 {
    [将 1 加入 $list:logs]
    [乙]
}
定义自制积木 乙 { [甲] }
定义自制积木 未调用的移动 { [移动 1 步] }
@sprite "玩家" {
    @init {}
    @list local = []
    定义自制积木 甲 { [将 2 加入 $list:local] }
    当绿旗被点击 {
        [甲]
        [乙]
        [将 $implicit 设为 1]
    }
}
"#,
        );
        let registry = Registry::new();
        assert!(bind_project(&project, &registry).is_ok());
        assert!(diagnose_project(&project, &registry).is_empty());

        // A public body may not capture a caller's private data declaration.
        let mut project = project;
        project.stage.lists.remove("logs");
        project.sprites[0].lists.insert("logs".into(), json!([]));
        let errors = diagnose_project(&project, &registry);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].0.contains("logs"));
        assert_eq!(
            bind_project(&project, &registry).err().unwrap().0,
            errors[0].0
        );
    }

    #[test]
    fn duplicate_procedure_headers_do_not_hide_body_errors() {
        let mut project = load(
            r#"
定义自制积木 同名 @first { [将 1 加入 $list:one] }
定义自制积木 另一名称 @second { [将 2 加入 $list:two] }
定义自制积木 后续 { [将 3 加入 $list:three] }
"#,
        );
        // IDs and slots normally come from the loader. Exercise a partially
        // recovered definition as well, without weakening strict compilation.
        project.public_procedures[1].template = project.public_procedures[0].template.clone();
        let errors = messages(&project);
        assert_eq!(errors.len(), 5, "{errors:?}");
        for expected in [
            "重复自制积木签名",
            "重复自制积木源符号",
            "one",
            "two",
            "three",
        ] {
            assert!(
                errors.iter().any(|error| error.contains(expected)),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn private_procedures_are_checked_even_when_unused() {
        let project = load(
            r#"
定义自制积木 公共 { [等待 1 秒] }
@sprite "玩家" {
    @init {}
    定义自制积木 私有 { [公共] }
}
@sprite "另一角色" {
    @init {}
    定义自制积木 私有 { [公共] }
}
"#,
        );
        let mut registry = Registry::new();
        for template in &mut registry.templates {
            if template.opcode == "control_wait" {
                template.stage_only = true;
            }
        }
        let errors = diagnose_project(&project, &registry);
        // The same public source is instantiated by two private callers; one
        // source diagnostic is sufficient for its target restriction.
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0]
                .0
                .contains("角色不支持仅舞台可用的积木 control_wait")
        );
    }
}
