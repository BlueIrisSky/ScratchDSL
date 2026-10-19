use sbdsl::{Error, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const HELP: &str = concat!(
    "ScratchDSL ",
    env!("CARGO_PKG_VERSION"),
    "\n\n",
    "sbdsl init [目录]\n",
    "  sbdsl build [sbdsl.buildsb | 文件.sbdsl] [-o 输出.sb3]\n",
    "  sbdsl check [sbdsl.buildsb | 文件.sbdsl]\n",
    "  sbdsl lsp --stdio\n",
    "  sbdsl --version",
);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        println!("{HELP}");
        return Ok(());
    };
    match command.to_str() {
        Some("lsp") => {
            if args.next().as_deref() != Some(std::ffi::OsStr::new("--stdio"))
                || args.next().is_some()
            {
                return Err(Error::new("语言服务器用法：sbdsl lsp --stdio"));
            }
            sbdsl::lsp::run_stdio()
        }
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("sbdsl {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("init") => {
            let dir = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            if args.next().is_some() {
                return Err(Error::new("init 只接受一个目录"));
            }
            init(&dir)?;
            println!("已初始化 {}", dir.canonicalize()?.display());
            Ok(())
        }
        Some("build" | "check") => {
            let check = command == "check";
            let mut input = None;
            let mut output = None;
            while let Some(arg) = args.next() {
                if arg == "-o" || arg == "--output" {
                    if check {
                        return Err(Error::new("check 不生成文件，不接受 -o"));
                    }
                    if output.is_some() {
                        return Err(Error::new("重复 -o 参数"));
                    }
                    output = Some(PathBuf::from(
                        args.next().ok_or_else(|| Error::new("-o 缺少输出文件"))?,
                    ));
                } else if arg == "--help" || arg == "-h" {
                    println!("{HELP}");
                    return Ok(());
                } else if arg.to_string_lossy().starts_with('-') {
                    return Err(Error::new(format!("未知选项 {}", arg.to_string_lossy())));
                } else if input.replace(PathBuf::from(arg)).is_some() {
                    return Err(Error::new("只能指定一个构建入口"));
                }
            }
            let config =
                sbdsl::config::discover(input.as_deref(), output.as_deref(), &env::current_dir()?)?;
            let compiled = sbdsl::build::compile(&config)?;
            let targets = compiled.json["targets"].as_array().map_or(0, |a| a.len());
            let blocks: usize = compiled.json["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| t["blocks"].as_object().map_or(0, |m| m.len()))
                .sum();
            if check {
                println!("检查通过：{targets} 个目标，{blocks} 个积木");
            } else {
                sbdsl::build::write_sb3(&compiled, &config.output)?;
                println!(
                    "已生成 {}（{targets} 个目标，{blocks} 个积木）",
                    config.output.display()
                );
            }
            Ok(())
        }
        _ => Err(Error::new(format!(
            "未知命令 {}\n\n{HELP}",
            command.to_string_lossy()
        ))),
    }
}

fn init(dir: &Path) -> Result<()> {
    sbdsl::config::local_path(dir)?;
    for path in [
        dir.join("sbdsl.buildsb"),
        dir.join("src/main.sbdsl"),
        dir.join("assets/sbdsl.svg"),
    ] {
        if path.exists() {
            return Err(Error::new(format!(
                "初始化不会覆盖已有文件 {}",
                path.display()
            )));
        }
    }
    for sub in ["src", "assets", "extensions"] {
        fs::create_dir_all(dir.join(sub))?;
    }
    fs::write(
        dir.join("sbdsl.buildsb"),
        "entry: \"./src/main.sbdsl\"\noutput: \"./dist/project.sb3\"\n",
    )?;
    fs::write(
        dir.join("src/main.sbdsl"),
        include_str!("../templates/main.sbdsl"),
    )?;
    fs::write(
        dir.join("assets/sbdsl.svg"),
        // Use the language icon on a larger canvas suitable for a costume.
        include_str!("../../vscode/icons/sbdsl-light.svg")
            .replace("width=\"16\" height=\"16\"", "width=\"96\" height=\"96\""),
    )?;
    Ok(())
}
