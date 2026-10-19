use crate::{
    Error, Result,
    analysis::{DiskSource, SourceProvider},
    config,
    model::*,
    parser,
    registry::procedure_template,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub fn load(entry: &Path, src_dir: &Path) -> Result<Project> {
    load_with_provider(entry, src_dir, &DiskSource)
}

pub fn load_with_provider(
    entry: &Path,
    src_dir: &Path,
    provider: &dyn SourceProvider,
) -> Result<Project> {
    let mut loader = Loader::new(src_dir, provider, false);
    loader.file(entry, true)?;
    Ok(loader.project)
}

/// A partial project built from every reachable module. Recovered nodes retain
/// their original scope; diagnostics and visited files remain available too.
#[derive(Debug)]
pub struct ProjectReport {
    pub project: Project,
    pub errors: Vec<Error>,
    pub files: BTreeSet<PathBuf>,
}

pub fn load_recovering(
    entry: &Path,
    src_dir: &Path,
    provider: &dyn SourceProvider,
) -> ProjectReport {
    let mut loader = Loader::new(src_dir, provider, true);
    if let Err(error) = loader.file(entry, true) {
        loader.errors.push(error);
    }
    ProjectReport {
        project: loader.project,
        errors: loader.errors,
        files: loader.visited,
    }
}

struct Loader<'a> {
    project: Project,
    visited: BTreeSet<PathBuf>,
    src_dir: PathBuf,
    next_proc: usize,
    provider: &'a dyn SourceProvider,
    recovering: bool,
    errors: Vec<Error>,
}

impl<'a> Loader<'a> {
    fn new(src_dir: &Path, provider: &'a dyn SourceProvider, recovering: bool) -> Self {
        let mut project = Project::default();
        project.stage.name = "Stage".into();
        Self {
            project,
            visited: BTreeSet::new(),
            src_dir: src_dir.to_path_buf(),
            next_proc: 0,
            provider,
            recovering,
            errors: Vec::new(),
        }
    }

    fn file(&mut self, path: &Path, entry: bool) -> Result<()> {
        let canonical = self.provider.identity(path);
        if !self.visited.insert(canonical.clone()) {
            return Ok(());
        }
        let source = self
            .provider
            .read_to_string(&canonical)
            .map_err(|error| error.in_file(&canonical))?;
        let items = if self.recovering {
            let report = parser::parse_recovering(&canonical, &source);
            self.errors.extend(report.errors);
            report.items
        } else {
            parser::parse(&canonical, &source)?
        };
        self.items(items, &canonical, entry, None)
    }
    fn items(
        &mut self,
        items: Vec<Item>,
        file: &Path,
        entry: bool,
        sprite: Option<usize>,
    ) -> Result<()> {
        for item in items {
            if let Err(error) = self.item(item, file, entry, sprite) {
                if self.recovering {
                    self.errors.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn item(&mut self, item: Item, file: &Path, entry: bool, sprite: Option<usize>) -> Result<()> {
        match item {
            Item::Import { module, span } => {
                let relative = format!("{}.sbdsl", module.replace('.', "/"));
                let nearby = file.parent().unwrap_or(Path::new(".")).join(&relative);
                let fallback = self.src_dir.join(&relative);
                let target = if self.provider.is_file(&nearby) {
                    nearby
                } else if self.provider.is_file(&fallback) {
                    fallback
                } else {
                    return Err(Error::new(format!(
                        "找不到模块 {module}；已查找 {} 和 {}",
                        nearby.display(),
                        fallback.display()
                    ))
                    .at(&span));
                };
                self.file(&target, false)?;
            }
            Item::Stage(children) => self.items(children, file, entry, None)?,
            Item::Sprite {
                name,
                add,
                mut init,
                items,
                span,
            } => {
                let index = if let Some(index) =
                    self.project.sprites.iter().position(|s| s.name == name)
                {
                    if !add {
                        let previous = self.project.sprites[index]
                            .declaration_span
                            .as_ref()
                            .map(|s| format!("{}:{}:{}", s.file, s.line, s.column))
                            .unwrap_or_else(|| "未知位置".into());
                        return Err(Error::new(format!(
                                "重复创建角色 {name:?}；此前定义在 {previous}。请使用 @sprite \"角色名\" add {{ ... }} 追加逻辑"
                            ))
                            .at(&span));
                    }
                    index
                } else {
                    if add {
                        return Err(Error::new(format!("add 的角色 {name:?} 尚未定义")).at(&span));
                    }
                    if init.is_none() && !self.recovering {
                        return Err(Error::new("创建角色必须以 @init { ... } 开头").at(&span));
                    }
                    // Bind resources to the declaring module, before any imports or
                    // later add blocks can change the surrounding project context.
                    // Recovery keeps the target after a broken @init, so its
                    // children can still be checked with private scope intact.
                    if let Some(properties) = init.as_mut() {
                        for asset in properties.costumes.iter_mut().chain(&mut properties.sounds) {
                            match config::resolve(
                                file.parent().unwrap_or(Path::new(".")),
                                &asset.path,
                            ) {
                                Ok(path) => asset.path = path,
                                Err(error) if self.recovering => {
                                    self.errors.push(error.at(&span));
                                }
                                Err(error) => return Err(error.at(&span)),
                            }
                        }
                    }
                    self.project.sprites.push(Target {
                        name,
                        init,
                        declaration_span: Some(span.clone()),
                        ..Target::default()
                    });
                    self.project.sprites.len() - 1
                };
                self.items(items, file, entry, Some(index))?;
            }
            Item::Data {
                name,
                value,
                list,
                global,
                ..
            } => {
                let target = match (global, sprite) {
                    (false, Some(index)) => &mut self.project.sprites[index],
                    _ => &mut self.project.stage,
                };
                if list {
                    target.lists.insert(name, value);
                } else {
                    target.variables.insert(name, value);
                }
            }
            Item::Procedure { header, body, span } => {
                self.next_proc += 1;
                let template = procedure_template(&header, &format!("p{}", self.next_proc))
                    .map_err(|e| e.at(&span))?;
                let procedures = if let Some(i) = sprite {
                    &mut self.project.sprites[i].procedures
                } else {
                    &mut self.project.public_procedures
                };
                if let Some(previous) = procedures
                    .iter()
                    .find(|p| p.template.signature() == template.signature())
                {
                    return Err(Error::new(format!(
                        "重复自制积木签名；此前定义在 {}:{}:{}",
                        previous.span.file, previous.span.line, previous.span.column
                    ))
                    .at(&span));
                }
                procedures.push(Procedure {
                    template,
                    body,
                    span,
                });
            }
            Item::Script { header, body, span } => {
                if let Some(i) = sprite {
                    self.project.sprites[i].scripts.push((header, body, span));
                } else if entry {
                    self.project.stage.scripts.push((header, body, span));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
