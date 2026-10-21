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
        let parsed = binder
            .parse_invocation(source, shape, context)
            .or_else(|typed_error| {
                // An invalid data type must not hide independent parameter errors
                // in the same call. Typed parsing still decides valid ambiguities.
                binder
                    .registry
                    .parse(source, shape, &context.visible)
                    .map_err(|_| typed_error)
            });
        match parsed {
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
        for (slot, expression) in invocation.template.slots().zip(&mut invocation.args) {
            if let Expr::Call(call) = expression {
                self.bind_invocation(binder, call, context, span);
            } else if let Err(error) =
                binder.bind_expression(expression, context, Some(slot.arg_type))
            {
                self.push(error.at(span));
                continue;
            }
            if let Err(error) = validate_bound_data_slot(slot, expression) {
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
                Statement::ListAssignment {
                    name,
                    global,
                    values,
                    span,
                } => match binder.list_assignment(name, *global, values, context, span) {
                    Ok(statements) => {
                        calls.extend(statements.into_iter().map(|statement| CheckedCall {
                            invocation: statement.invocation,
                            span: statement.span,
                        }))
                    }
                    Err(error) => self.push(error),
                },
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
                Statement::ExtensionControl {
                    header,
                    branches,
                    span,
                } => {
                    if let Some(call) =
                        self.invocation(binder, header, Shape::Control, context, span)
                    {
                        if let Err(error) =
                            validate_extension_branches(&call.invocation.template, branches.len())
                        {
                            self.push(error.at(span));
                        }
                        calls.push(call);
                    }
                    for branch in branches {
                        self.statements(binder, branch, context, calls);
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
    if let Err(error) = validate_project_data_names(&binder.targets) {
        diagnostics.push(error);
    }
    // Infer all assignment targets before resolving values, including forward
    // references. Invalid assignments are diagnosed at their original statement.
    let _ = binder.infer_assignment_types(project);
    let mut procedures = BTreeMap::<String, CheckedProcedure>::new();
    for (scope, definitions) in std::iter::once((None, project.public_procedures.as_slice())).chain(
        std::iter::once(&project.stage)
            .chain(&project.sprites)
            .enumerate()
            .map(|(index, target)| (Some(index), target.procedures.as_slice())),
    ) {
        let mut signatures = BTreeSet::new();
        for definition in definitions {
            if !signatures.insert(definition.template.signature()) {
                diagnostics.push(Error::new("同一作用域存在重复自制积木签名").at(&definition.span));
            }
            let context = binder.context(scope, Some(definition));
            let mut calls = Vec::new();
            diagnostics.statements(&mut binder, &definition.body, &context, &mut calls);
            // Private procedures are emitted on their owner even when uncalled.
            // Checking them here also covers malformed/duplicate definition IDs.
            if let Some(index) = scope {
                diagnostics.calls_target(&calls, index);
            }
            let Some(id) = definition.template.procedure.clone() else {
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
                        span: definition.span.clone(),
                        calls,
                    });
                }
            }
        }
    }

    for (index, target) in std::iter::once(&project.stage)
        .chain(&project.sprites)
        .enumerate()
    {
        let context = binder.context(Some(index), None);
        let mut calls = Vec::new();
        for (header, body, span) in &target.scripts {
            if let Some(call) =
                diagnostics.invocation(&mut binder, header, Shape::Hat, &context, span)
            {
                calls.push(call);
            }
            diagnostics.statements(&mut binder, body, &context, &mut calls);
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
            r#"未知事件:
    第一处未知积木
    if 1:
        将 (1) 加入 (then_missing)
    else:
        将 (2) 加入 (else_missing)
    value = @outside
start:
    第二处未知积木
    移动 (1) 步
"#,
        );
        let errors = diagnose_project(&project, &Registry::new());
        for expected in [
            "未知事件",
            "第一处未知积木",
            "if 1",
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
            r#"def 检查 @n <@ok>:
    implicit = (@first + @second)
    if <@n>:
        pass
    等待 (@ok) 秒
    将 (1) 加入 (missing)
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
        let project = load("start:\n    移动 (@missing) 步");
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
            r#"def 甲:
    乙
    移动 (1) 步
def 乙:
    甲
    x = (x 坐标)
start:
    甲
    甲
sprite "玩家":
    init:
        pass
    start:
        乙
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
            r#"logs = {}
def 甲:
    将 (1) 加入 (logs)
    乙
def 乙:
    甲
def 未调用的移动:
    移动 (1) 步
sprite "玩家":
    init:
        pass
    local = {}
    def 甲:
        将 (2) 加入 (local)
    start:
        甲
        乙
        implicit = 1
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
        // Typed template selection adds matching context to the strict error;
        // recovery still reports the missing list while checking other slots.
        let strict = bind_project(&project, &registry).err().unwrap().0;
        assert!(
            strict.contains("logs") && strict.contains("未声明"),
            "{strict}"
        );
    }

    #[test]
    fn duplicate_procedure_headers_do_not_hide_body_errors() {
        let mut project = load(
            r#"def 同名 @first:
    将 (1) 加入 (one)
def 另一名称 @second:
    将 (2) 加入 (two)
def 后续:
    将 (3) 加入 (three)
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
            r#"def 公共:
    等待 (1) 秒
sprite "玩家":
    init:
        pass
    def 私有:
        公共
sprite "另一角色":
    init:
        pass
    def 私有:
        公共
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
