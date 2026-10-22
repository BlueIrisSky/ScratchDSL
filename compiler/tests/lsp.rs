use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Client {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<Value>,
    next_id: u32,
}

impl Client {
    fn start(allow_extensions: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sbdsl"))
            .args(["lsp", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut length = None;
                loop {
                    let mut header = String::new();
                    if stdout.read_line(&mut header).unwrap() == 0 {
                        return;
                    }
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(size) = header.strip_prefix("Content-Length: ") {
                        length = Some(size.trim().parse::<usize>().unwrap());
                    } else {
                        assert!(
                            header.starts_with("Content-Type:"),
                            "non-protocol stdout: {header}"
                        );
                    }
                }
                let mut bytes = vec![0; length.expect("Content-Length")];
                stdout.read_exact(&mut bytes).unwrap();
                if tx.send(serde_json::from_slice(&bytes).unwrap()).is_err() {
                    return;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            messages,
            next_id: 1,
        };
        let result = client.request("initialize", json!({"processId": null, "rootUri": null, "capabilities": {}, "initializationOptions": {"allowExtensions": allow_extensions}}));
        assert_eq!(result["capabilities"]["positionEncoding"], "utf-16");
        let triggers = result["capabilities"]["completionProvider"]["triggerCharacters"]
            .as_array()
            .unwrap();
        assert!(
            !triggers.contains(&json!("["))
                && !triggers.contains(&json!("$"))
                && triggers.contains(&json!(":"))
        );
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        client.notify("initialized", json!({}));
        client
    }
    fn send(&mut self, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        self.input.write_all(&bytes).unwrap();
        self.input.flush().unwrap();
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0", "method":method, "params":params}));
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
        let message = self.wait(|message| message["id"] == id);
        assert!(message.get("error").is_none(), "{message}");
        message["result"].clone()
    }
    fn wait(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut observed = Vec::new();
        loop {
            let message = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!("LSP timeout/disconnect: {error}; observed: {observed:?}")
                });
            if predicate(&message) {
                return message;
            }
            observed.push(message);
        }
    }
    fn open(&mut self, uri: &str, language: &str, source: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":language,"version":1,"text":source}}),
        );
    }
    fn change(&mut self, uri: &str, version: i32, source: &str) {
        self.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source}]}));
    }
    fn diagnostics(&mut self, uri: &str, version: i32, has_error: bool) -> Value {
        self.wait(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
                && message["params"]["version"] == version
                && message["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty() == has_error)
        })["params"]
            .clone()
    }
    fn complete(&mut self, uri: &str, version: i32, marked: &str) -> Vec<Value> {
        let cursor = marked.find('|').expect("completion cursor");
        let prefix = &marked[..cursor];
        let position = json!({
            "line": prefix.bytes().filter(|&byte| byte == b'\n').count(),
            "character": prefix.rsplit('\n').next().unwrap().encode_utf16().count()
        });
        self.change(uri, version, &marked.replacen('|', "", 1));
        self.request(
            "textDocument/completion",
            json!({"textDocument":{"uri":uri},"position":position}),
        )["items"]
            .as_array()
            .unwrap()
            .clone()
    }
    fn stop(mut self) {
        assert!(self.request("shutdown", Value::Null).is_null());
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "LSP failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn uri(path: &std::path::Path) -> String {
    url::Url::from_file_path(path).unwrap().into()
}
fn is_file_diagnostic(message: &Value, path: &std::path::Path) -> bool {
    message["method"] == "textDocument/publishDiagnostics"
        && message["params"]["uri"]
            .as_str()
            .and_then(|uri| url::Url::parse(uri).ok())
            .and_then(|uri| uri.to_file_path().ok())
            .is_some_and(|actual| {
                sbdsl::analysis::normalize_path(&actual) == sbdsl::analysis::normalize_path(path)
            })
}
const VALID: &str =
    "sprite \"玩家😀\":\n    init:\n        pass\n    start:\n        移动 (10) 步\n    end";

#[test]
fn stdio_diagnostics_completion_versions_and_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("中文 空格.sbdsl");
    std::fs::write(&path, VALID).unwrap();
    let uri = uri(&path);
    let mut client = Client::start(false);
    client.open(&uri, "sbdsl", VALID);
    client.diagnostics(&uri, 1, false);
    client.change(&uri, 2, &VALID.replace("移动 (10) 步", "不存在的积木"));
    let diagnostics = client.diagnostics(&uri, 2, true);
    assert_eq!(diagnostics["diagnostics"][0]["range"]["start"]["line"], 4);
    client.change(&uri, 3, &VALID.replace("移动 (10) 步", "移"));
    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":4,"character":9}}),
    );
    let items = completion["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["label"].as_str().unwrap_or_default().contains("移动")),
        "{completion}"
    );
    client.change(&uri, 4, VALID);
    client.change(&uri, 3, "this stale notification must be ignored");
    client.diagnostics(&uri, 4, false);
    client.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
    client.wait(|m| {
        m["method"] == "textDocument/publishDiagnostics"
            && m["params"]["uri"] == uri
            && m["params"]["diagnostics"] == json!([])
    });
    client.stop();
}

#[test]
fn unsaved_config_new_import_close_and_watched_disk_change() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let library = directory.path().join("新模块.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    std::fs::write(&main, VALID).unwrap();
    let cfg = "entry: \"./main.sbdsl\"\n";
    std::fs::write(&config, cfg).unwrap();
    let (main_uri, library_uri, config_uri) = (uri(&main), uri(&library), uri(&config));
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", VALID);
    client.diagnostics(&main_uri, 1, false);
    client.open(&config_uri, "sbdsl-build", "{ broken config");
    client.diagnostics(&config_uri, 1, true);
    client.change(&config_uri, 2, cfg);
    client.diagnostics(&config_uri, 2, false);
    client.open(&library_uri, "sbdsl", "score = 2");
    let imported = format!("import 新模块\n{VALID}");
    client.change(&main_uri, 2, &imported);
    client.diagnostics(&main_uri, 2, false);
    client.notify(
        "textDocument/didClose",
        json!({"textDocument":{"uri":library_uri}}),
    );
    client.diagnostics(&main_uri, 2, true);
    std::fs::write(&library, "score = 3").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":library_uri,"type":1}]}),
    );
    client.diagnostics(&main_uri, 2, false);
    client.stop();
}

#[test]
fn untitled_file_has_syntax_diagnostics_and_completion() {
    let mut client = Client::start(false);
    let uri = "untitled:Untitled-1";
    client.open(
        uri,
        "sbdsl",
        "sprite \"玩家\":\n    init:\n        pass\n    start:\n        移动(",
    );
    client.diagnostics(uri, 1, true);
    client.change(
        uri,
        2,
        "sprite \"玩家\":\n    init:\n        pass\n    start:\n        移",
    );
    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":4,"character":9}}),
    );
    assert!(!completion["items"].as_array().unwrap().is_empty());
    client.stop();
}

#[test]
fn project_ast_diagnostics_include_multiple_errors_and_unopened_imports() {
    use std::collections::BTreeMap;

    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let module = directory.path().join("library.sbdsl");
    let (main_uri, module_uri) = (uri(&main), uri(&module));
    let source = "a = \nb = {1,}\nsprite \"玩家😀\":\n    init:\n        pass\n    start:\n        不存在的动作\n    end\nimport library\n";
    std::fs::write(&main, source).unwrap();
    std::fs::write(&module, "a = \nb = {1,}\n").unwrap();
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", source);
    let mut published = BTreeMap::new();
    while published.len() < 2 {
        let message = client.wait(|m| {
            m["method"] == "textDocument/publishDiagnostics"
                && (m["params"]["uri"] == main_uri || is_file_diagnostic(m, &module))
        });
        let params = &message["params"];
        let key = if is_file_diagnostic(&message, &module) {
            &module_uri
        } else {
            &main_uri
        };
        published.insert(key.clone(), params.clone());
    }
    let main_diagnostics = published[&main_uri]["diagnostics"].as_array().unwrap();
    assert_eq!(main_diagnostics.len(), 3, "{published:?}");
    let lines: Vec<_> = main_diagnostics
        .iter()
        .map(|d| d["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![0, 1, 6]);
    let module_diagnostics = &published[&module_uri];
    assert_eq!(
        module_diagnostics["diagnostics"].as_array().unwrap().len(),
        2
    );
    assert!(
        module_diagnostics.get("version").is_none(),
        "unopened file has no editor version"
    );

    // Removing the import also clears diagnostics for the still-broken disk file.
    client.change(&main_uri, 2, VALID);
    let mut cleared = BTreeMap::new();
    while cleared.len() < 2 {
        let message = client.wait(|m| {
            m["method"] == "textDocument/publishDiagnostics"
                && m["params"]["diagnostics"] == json!([])
                && (m["params"]["uri"] == main_uri || is_file_diagnostic(m, &module))
        });
        let params = &message["params"];
        let key = if is_file_diagnostic(&message, &module) {
            &module_uri
        } else {
            &main_uri
        };
        cleared.insert(key.clone(), params.clone());
    }
    assert_eq!(cleared[&main_uri]["version"], 2);
    client.change(&main_uri, 3, &format!("{VALID}\nimport library\n"));
    client.wait(|m| {
        m["method"] == "textDocument/publishDiagnostics"
            && is_file_diagnostic(m, &module)
            && m["params"]["diagnostics"]
                .as_array()
                .is_some_and(|errors| errors.len() == 2)
    });
    std::fs::write(&module, "a = 0\nb = {}\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":module_uri,"type":2}]}),
    );
    client.wait(|m| {
        m["method"] == "textDocument/publishDiagnostics"
            && is_file_diagnostic(m, &module)
            && m["params"]["diagnostics"] == json!([])
    });
    client.stop();
}

#[test]
fn unopened_diagnostics_keep_project_directory_alias_and_open_uri() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real project");
    let alias = directory.path().join("project alias");
    std::fs::create_dir_all(real.join("shared")).unwrap();
    #[cfg(windows)]
    {
        // Directory junctions need no symlink privilege on Windows runners.
        let result = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&alias)
            .arg(&real)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let source = format!("{VALID}\nimport shared.library\n");
    let main = alias.join("main.sbdsl");
    let module = alias.join("shared/library.sbdsl");
    std::fs::write(&main, &source).unwrap();
    std::fs::write(&module, "invalid = \ninvalidList = {1,}\n").unwrap();
    let main_uri = uri(&main);
    let alias_module_uri = uri(&module);
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", &source);
    let diagnostic = client.wait(|m| {
        m["method"] == "textDocument/publishDiagnostics"
            && m["params"]["uri"] == alias_module_uri
            && m["params"]["diagnostics"]
                .as_array()
                .is_some_and(|items| items.len() == 2)
    });
    assert!(diagnostic["params"].get("version").is_none());

    // Watched changes and removal must clear the same editor-visible identity.
    std::fs::write(&module, "valid = 0\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":alias_module_uri,"type":2}]}),
    );
    client.wait(|m| {
        m["method"] == "textDocument/publishDiagnostics"
            && m["params"]["uri"] == alias_module_uri
            && m["params"]["diagnostics"] == json!([])
    });

    // If the module is opened under another spelling, its exact document URI
    // takes priority over aliases inferred from importing documents.
    let direct_uri = uri(&real.join("shared/library.sbdsl"));
    client.open(&direct_uri, "sbdsl", "invalid = \n");
    client.diagnostics(&direct_uri, 1, true);
    client.stop();
}

#[test]
fn untitled_ast_diagnostics_report_and_clear_multiple_errors() {
    let mut client = Client::start(false);
    let uri = "untitled:AST-%E8%AF%8A%E6%96%AD";
    client.open(uri, "sbdsl", "a = \nb = {1,}\n");
    let result = client.diagnostics(uri, 1, true);
    assert_eq!(
        result["diagnostics"].as_array().unwrap().len(),
        2,
        "{result}"
    );
    client.change(uri, 2, "a = 0\nb = {}\n");
    client.diagnostics(uri, 2, false);
    client.stop();
}

#[test]
fn untitled_build_file_accepts_and_diagnoses_extension_aliases() {
    let mut client = Client::start(false);
    let uri = "untitled:aliases.buildsb";
    client.open(
        uri,
        "sbdsl-build",
        "extensions: [\"自定义拓展.js\" as \"扩展\"]\n",
    );
    client.diagnostics(uri, 1, false);
    client.change(uri, 2, "extensions: [\"自定义拓展.js\" as]\n");
    client.diagnostics(uri, 2, true);
    client.change(uri, 3, "extensions: [\"自定义拓展.js\" as \"扩展\"]\n");
    client.diagnostics(uri, 3, false);
    client.stop();
}

#[test]
fn slow_extension_does_not_block_completion_or_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    std::fs::write(&main, VALID).unwrap();
    std::fs::write(
        directory.path().join("sbdsl.buildsb"),
        r#"{"entry":"./main.sbdsl","extensions":["./慢扩展.js"]}"#,
    )
    .unwrap();
    std::fs::write(directory.path().join("慢扩展.js"), "while (true) {}").unwrap();
    let mut client = Client::start(true);
    let uri = uri(&main);
    client.open(&uri, "sbdsl", &VALID.replace("移动 (10) 步", "移"));
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":4,"character":9}}),
    );
    assert!(!completion["items"].as_array().unwrap().is_empty());
    client.stop();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "slow analysis blocked protocol thread"
    );
}

#[test]
fn unsaved_extension_alias_updates_diagnostics_and_completion() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    let config_source = "entry: \"main.sbdsl\"\nextensions: [\"自定义拓展.js\" as \"first\"]\n";
    let source = "start:\n    first.做\nend\n";
    std::fs::write(&main, source).unwrap();
    std::fs::write(&config, config_source).unwrap();
    std::fs::write(
        directory.path().join("自定义拓展.js"),
        "Scratch.extensions.register({getInfo(){return {id:'example',blocks:[{opcode:'run',blockType:'command',text:'做'}]}}});",
    )
    .unwrap();
    let (main_uri, config_uri) = (uri(&main), uri(&config));
    let mut client = Client::start(true);
    client.open(&config_uri, "sbdsl-build", config_source);
    client.open(&main_uri, "sbdsl", source);
    client.diagnostics(&main_uri, 1, false);
    let complete = |client: &mut Client, source: &str| {
        let line = source.lines().nth(1).unwrap_or("");
        client.request(
            "textDocument/completion",
            json!({
                "textDocument":{"uri":main_uri},
                "position":{"line":1,"character":line.encode_utf16().count()}
            }),
        )["items"]
            .as_array()
            .unwrap()
            .clone()
    };
    assert!(
        complete(&mut client, source)
            .iter()
            .any(|item| item["label"] == "first.做")
    );

    // Editing only the alias in an open, unsaved build file must invalidate
    // completions as well as rebind every open source against its new name.
    client.change(&config_uri, 2, &config_source.replace("first", "second"));
    let diagnostics = client.diagnostics(&main_uri, 1, true);
    assert_eq!(diagnostics["diagnostics"].as_array().unwrap().len(), 1);
    assert!(complete(&mut client, source).is_empty());
    let renamed_source = source.replace("first", "second");
    client.change(&main_uri, 2, &renamed_source);
    client.diagnostics(&main_uri, 2, false);
    let items = complete(&mut client, &renamed_source);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["label"], "second.做");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), config_source);

    client.notify(
        "textDocument/didClose",
        json!({"textDocument":{"uri":config_uri}}),
    );
    client.diagnostics(&main_uri, 2, true);
    assert!(complete(&mut client, &renamed_source).is_empty());
    client.change(&main_uri, 3, source);
    client.diagnostics(&main_uri, 3, false);
    assert!(
        complete(&mut client, source)
            .iter()
            .any(|item| item["label"] == "first.做")
    );
    client.stop();
}

#[test]
fn extension_controls_complete_all_suites_and_publish_branch_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let valid = "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    else:\n        pass\n    end\nend\n";
    std::fs::write(&main, valid).unwrap();
    std::fs::write(
        directory.path().join("sbdsl.buildsb"),
        "entry: \"main.sbdsl\"\nextensions: [\"flow.js\"]\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("flow.js"),
        include_str!("fixtures/control-extension.js"),
    )
    .unwrap();
    let main_uri = uri(&main);
    let mut client = Client::start(true);
    client.open(&main_uri, "sbdsl", valid);
    client.diagnostics(&main_uri, 1, false);

    let items = client.complete(&main_uri, 2, "start:\n    flow.选|");
    let pick = items
        .iter()
        .find(|item| {
            item["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("flow_pick"))
        })
        .unwrap_or_else(|| panic!("{items:?}"));
    let snippet = pick["textEdit"]["newText"].as_str().unwrap();
    assert!(
        snippet.contains("第二") && snippet.contains("第三") && snippet.contains("结束"),
        "{snippet}"
    );
    assert_eq!(snippet.matches("\n    else:").count(), 2, "{snippet}");
    assert_eq!(snippet.matches(":pass}").count(), 3, "{snippet}");
    assert_eq!(
        pick["textEdit"]["range"],
        json!({"start":{"line":1,"character":4},"end":{"line":1,"character":10}})
    );

    let mut version = 3;
    for invalid in [
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    end\nend\n",
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    else:\n        pass\n    else:\n        pass\n    end\nend\n",
        "start:\n    flow.执行 (1) 次\nend\n",
        "start:\n    flow.如果 <true> 否则 (1):\n        pass\n    else:\n        删除 (missing) 的全部项目\n    end\nend\n",
    ] {
        client.change(&main_uri, version, invalid);
        let diagnostics = client.diagnostics(&main_uri, version, true);
        assert!(
            diagnostics["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["severity"] == 1),
            "{invalid}: {diagnostics}"
        );
        version += 1;
        client.change(&main_uri, version, valid);
        client.diagnostics(&main_uri, version, false);
        version += 1;
    }
    client.stop();
}

#[test]
fn symbols_follow_current_unsaved_imports_and_sprite_scope() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("main.sbdsl");
    let library = directory.path().join("未保存模块.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    let source = "shared = 1\nsprite \"玩家😀\":\n    init:\n        pass\n    shared = 2\n    start:\n        说(|)\nimport 未保存模块\n";
    std::fs::write(&main, source.replace('|', "")).unwrap();
    std::fs::write(&config, r#"{"entry":"main.sbdsl"}"#).unwrap();
    let (main_uri, library_uri) = (uri(&main), uri(&library));
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", &source.replace('|', ""));
    client.open(
        &library_uri,
        "sbdsl",
        "imported = 3\nlogs = {}\ndef 公共动作:\n    等待 (0) 秒\nsprite \"玩家😀\" add:\n    hp = 100\n    inventory = {}\n    def 私有动作:\n        等待 (0) 秒\nsprite \"其他角色\":\n    init:\n        pass\n    secret = 4\n    def 其他动作:\n        等待 (0) 秒",
    );
    // Request immediately: symbol completion must not wait for diagnostics or a
    // successful parse of the unfinished variable in the active document.
    let items = client.complete(&main_uri, 2, source);
    for label in ["shared", "imported", "hp"] {
        assert!(
            items.iter().any(|item| item["label"] == label),
            "{label}: {items:?}"
        );
    }
    assert_eq!(
        items
            .iter()
            .filter(|item| item["label"] == "shared")
            .count(),
        1
    );
    assert!(!items.iter().any(|item| item["label"] == "secret"));

    assert!(
        !items
            .iter()
            .any(|item| item["label"] == "logs" || item["label"] == "inventory")
    );
    let items = client.complete(&main_uri, 3, &source.replace("说(|)", "将(\"x\")加入(|)"));
    for label in ["logs", "inventory"] {
        assert!(
            items.iter().any(|item| item["label"] == label),
            "{label}: {items:?}"
        );
    }
    assert!(!items.iter().any(|item| item["label"] == "shared"));
    let items = client.complete(&main_uri, 4, &source.replace("说(|)", "说(global:|)"));
    assert!(items.iter().any(|item| item["label"] == "global:shared"));
    assert!(items.iter().any(|item| item["label"] == "global:imported"));
    assert!(!items.iter().any(|item| item["label"] == "global:hp"));
    let items = client.complete(
        &main_uri,
        5,
        &source.replace("说(|)", "将(\"x\")加入(global:|)"),
    );
    assert!(items.iter().any(|item| item["label"] == "global:logs"));
    assert!(!items.iter().any(|item| item["label"] == "global:inventory"));

    let items = client.complete(&main_uri, 6, &source.replace("说(|)", "私|"));
    assert!(
        items.iter().any(|item| item["label"] == "私有动作"),
        "{items:?}"
    );
    let items = client.complete(&main_uri, 7, &source.replace("说(|)", "其他|"));
    assert!(items.is_empty(), "{items:?}");

    client.change(&library_uri, 2, "renamed = 4\n");
    let items = client.complete(&main_uri, 8, source);
    assert!(items.iter().any(|item| item["label"] == "renamed"));
    assert!(!items.iter().any(|item| item["label"] == "imported"));
    client.notify(
        "textDocument/didClose",
        json!({"textDocument":{"uri":library_uri}}),
    );
    let items = client.complete(&main_uri, 9, source);
    assert!(!items.iter().any(|item| item["label"] == "renamed"));
    client.stop();
}

#[test]
fn symbol_completion_uses_unsaved_config_and_public_procedure_scope() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("sbdsl.buildsb");
    let entry = directory.path().join("entry.sbdsl");
    let active = directory.path().join("active.sbdsl");
    std::fs::write(&config, r#"{"entry":"old.sbdsl"}"#).unwrap();
    std::fs::write(directory.path().join("old.sbdsl"), "stale = 1").unwrap();
    std::fs::write(&entry, "from_entry = 1\nimport active").unwrap();
    let source = "def 公共动作 @消息 <@条件>:\n    说(|)\nsprite \"玩家\":\n    init:\n        pass\n    private = 1\n    def 私有动作:\n        等待 (0) 秒";
    std::fs::write(&active, source.replace('|', "")).unwrap();
    let mut client = Client::start(false);
    let (config_uri, active_uri) = (uri(&config), uri(&active));
    client.open(&active_uri, "sbdsl", &source.replace('|', ""));
    client.open(&config_uri, "sbdsl-build", "entry: \"entry.sbdsl\"\n");
    let items = client.complete(&active_uri, 2, source);
    assert!(
        items.iter().any(|item| item["label"] == "from_entry"),
        "{items:?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item["label"] == "stale" || item["label"] == "private")
    );
    let items = client.complete(&active_uri, 3, &source.replace("说(|)", "说(@消|)"));
    assert!(
        items.iter().any(|item| item["label"] == "@消息"),
        "{items:?}"
    );
    let items = client.complete(&active_uri, 4, &source.replace("说(|)", "等待直到 <@条|>"));
    assert!(
        items.iter().any(|item| item["label"] == "<@条件>"),
        "{items:?}"
    );
    let items = client.complete(&active_uri, 5, &source.replace("说(|)", "私|"));
    assert!(items.is_empty(), "{items:?}");
    client.stop();
}

#[test]
fn operator_completion_preserves_scope_and_comparison_delimiters() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("运算😀.sbdsl");
    let config = directory.path().join("sbdsl.buildsb");
    std::fs::write(&config, "entry: \"运算😀.sbdsl\"\n").unwrap();
    let main_uri = uri(&main);
    let source = |body: &str| {
        format!(
            "shared = 1\nsprite \"玩家😀\":\n    init:\n        pass\n    score = 2\n    def 检查 @steps <@ready>:\n        {body}\nsprite \"其他\":\n    init:\n        pass\n    hidden = 3"
        )
    };
    let initial = source("等待 (0) 秒");
    std::fs::write(&main, &initial).unwrap();
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", &initial);
    let mut version = 2;
    for expression in [
        "<score > |>",
        "<score>|>",
        "<score\n>\n|>",
        "((score) / |)",
        "((score)/|)",
        "((score)\n/\n|)",
        "((score) % |)",
        "((score)+|)",
        "((score)-|)",
        "((score)*|)",
        "<score=|>",
        "<score<|>",
    ] {
        let marked = source(&format!("说({expression})"));
        let items = client.complete(&main_uri, version, &marked);
        version += 1;
        for label in ["score", "shared"] {
            assert!(
                items.iter().any(|item| item["label"] == label),
                "{expression}: {items:?}"
            );
        }
        assert!(
            !items.iter().any(|item| item["label"] == "hidden"),
            "{expression}"
        );
        let score = items.iter().find(|item| item["label"] == "score").unwrap();
        assert_eq!(score["detail"], "角色变量");
    }
    for (expression, label, insertion) in [
        ("<@st|旧>1>", "@steps", "@steps"),
        ("<@re|旧 > 1>", "<@ready>", "<@ready>"),
    ] {
        let marked = source(&format!("# 😀 # 说({expression})"));
        let items = client.complete(&main_uri, version, &marked);
        version += 1;
        let selected = items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("{items:?}"));
        let at = marked[..marked.find('|').unwrap()].rfind('@').unwrap();
        let line = marked[..at].bytes().filter(|&byte| byte == b'\n').count();
        let start = marked[..at]
            .rsplit('\n')
            .next()
            .unwrap()
            .encode_utf16()
            .count();
        assert_eq!(
            selected["textEdit"]["range"]["start"],
            json!({"line":line,"character":start})
        );
        assert_eq!(
            selected["textEdit"]["range"]["end"],
            json!({"line":line,"character":start+4})
        );
        assert_eq!(selected["textEdit"]["newText"], insertion);
    }
    let marked = source("说 <ext.模板 <@ready> (1)>\n  说(|)");
    let items = client.complete(&main_uri, version, &marked);
    assert!(
        items.iter().any(|item| item["label"] == "score"),
        "{items:?}"
    );
    assert!(!items.iter().any(|item| item["label"] == "hidden"));
    client.stop();
}

#[test]
fn assignment_completion_and_diagnostics_use_script_scope_and_utf16_targets() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("赋值😀.sbdsl");
    let source = |body: &str| {
        format!(
            "score = 1\nlogs = {{}}\nsprite \"玩家😀\":\n    init:\n        pass\n    score = 2\n    特殊分数 = 3\n    def 更新 @steps:\n        {body}\n    end\nsprite \"其他\":\n    init:\n        pass\n    hidden = 4"
        )
    };
    let initial = source("score += @steps * 2");
    std::fs::write(&main, &initial).unwrap();
    let main_uri = uri(&main);
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", &initial);
    client.diagnostics(&main_uri, 1, false);
    let mut version = 2;
    for (body, label, detail) in [
        ("sc|旧 = 10", "score", "角色变量"),
        ("global:sc|旧 += 1", "global:score", "全局变量"),
        ("score = sc|旧 + @steps", "score", "角色变量"),
        ("score *= 2 + sc|旧", "score", "角色变量"),
        ("score %= |", "score", "角色变量"),
        ("score = @st|旧", "@steps", "字符串 / 数值参数"),
        ("# 😀 # 特殊分|旧 /= 2", "特殊分数", "角色变量"),
    ] {
        let marked = source(body);
        let items = client.complete(&main_uri, version, &marked);
        version += 1;
        let selected = items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("{body}: {items:?}"));
        assert_eq!(selected["detail"], detail, "{body}");
        assert!(
            !items.iter().any(|item| item["label"] == "hidden"),
            "{body}"
        );
        if body.starts_with('#') {
            let at = marked.rfind("特殊分").unwrap();
            let character = marked[..at]
                .rsplit('\n')
                .next()
                .unwrap()
                .encode_utf16()
                .count();
            assert_eq!(
                selected["textEdit"]["range"]["start"],
                json!({"line": 8, "character": character})
            );
            assert_eq!(
                selected["textEdit"]["range"]["end"]["character"],
                character + "特殊分旧".encode_utf16().count()
            );
        }
    }
    for body in ["lo|gs = 1", "score = lo|gs", "@st| = 1"] {
        let items = client.complete(&main_uri, version, &source(body));
        version += 1;
        assert!(items.is_empty(), "{body}: {items:?}");
    }
    let items = client.complete(&main_uri, version, &source("score +|"));
    version += 1;
    assert!(items.iter().any(|item| item["label"] == "+="));
    client.change(&main_uri, version, &source("将 (score) 设为 (10)"));
    client.diagnostics(&main_uri, version, true);
    client.stop();
}

#[test]
fn data_name_conflicts_and_scalar_list_misuse_publish_and_clear_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("data_types.sbdsl");
    let valid = "score = 1\nlogs = {}\nstart:\n    score = 2\n    将 (score) 加入 (logs)\nend";
    std::fs::write(&main, valid).unwrap();
    let main_uri = uri(&main);
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", valid);
    client.diagnostics(&main_uri, 1, false);
    let mut version = 2;
    for invalid in [
        "score = 1\nscore = {}\nstart:\n    pass\nend",
        "score = 1\nlogs = {}\nstart:\n    logs = 1\nend",
        "score = 1\nlogs = {}\nstart:\n    score = logs\nend",
        "score = 1\nlogs = {}\nstart:\n    将 (\"x\") 加入 (score)\nend",
        "score = 1\nlogs = {}\nstart:\n    说 (logs)\nend",
        "score = 1\nsprite \"玩家\":\n    init:\n        pass\n    score = {}\n    start:\n        pass\n    end",
        "var \"bad name\" = 1\nstart:\n    pass\nend",
        "list \"bad list\" = {}\nstart:\n    pass\nend",
    ] {
        client.change(&main_uri, version, invalid);
        let diagnostics = client.diagnostics(&main_uri, version, true);
        assert!(
            diagnostics["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["severity"] == 1),
            "{invalid}: {diagnostics}"
        );
        version += 1;
        client.change(&main_uri, version, valid);
        client.diagnostics(&main_uri, version, false);
        version += 1;
    }
    client.stop();
}

#[test]
fn first_script_assignments_infer_completion_kinds_and_invalid_changes_clear() {
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("inferred_data.sbdsl");
    let source =
        |body: &str| format!("start:\n    score = 1\n    logs = {{\"first\"}}\n    {body}\nend");
    let valid = source("logs = {}\n    将(score)加入(logs)");
    std::fs::write(&main, &valid).unwrap();
    let main_uri = uri(&main);
    let mut client = Client::start(false);
    client.open(&main_uri, "sbdsl", &valid);
    client.diagnostics(&main_uri, 1, false);
    for (version, (body, expected, excluded)) in (2..).zip([
        ("等待(|)秒", "score", "logs"),
        ("将(score)加入(|)", "logs", "score"),
        ("lo|旧 = {1}", "logs", "score"),
        ("score = {}\n    等待(|)秒", "score", "logs"),
        ("logs = 1\n    将(score)加入(|)", "logs", "score"),
    ]) {
        let items = client.complete(&main_uri, version, &source(body));
        assert!(
            items.iter().any(|item| item["label"] == expected),
            "{body}: {items:?}"
        );
        assert!(
            !items.iter().any(|item| item["label"] == excluded),
            "{body}: {items:?}"
        );
    }
    client.stop();
}

#[test]
fn opening_only_a_build_file_analyzes_the_project_and_completes_fields() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("sbdsl.buildsb");
    let main = directory.path().join("main.sbdsl");
    std::fs::write(&main, "broken = \nalsoBroken = {1,}\n").unwrap();
    let config_uri = uri(&config);
    let mut client = Client::start(false);
    client.open(
        &config_uri,
        "sbdsl-build",
        "entry: \"main.sbdsl\"\nunknown: 1\n",
    );
    let error = client.diagnostics(&config_uri, 1, true);
    assert_eq!(error["diagnostics"][0]["range"]["start"]["line"], 1);
    client.change(&config_uri, 2, "entry: \"main.sbdsl\"\n");
    let mut config_cleared = false;
    let mut main_analyzed = false;
    while !config_cleared || !main_analyzed {
        let message = client.wait(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && (message["params"]["uri"] == config_uri || is_file_diagnostic(message, &main))
        });
        if message["params"]["uri"] == config_uri {
            assert_eq!(message["params"]["version"], 2);
            assert_eq!(message["params"]["diagnostics"], json!([]));
            config_cleared = true;
        } else {
            assert_eq!(
                message["params"]["diagnostics"].as_array().unwrap().len(),
                2
            );
            main_analyzed = true;
        }
    }
    let items = client.complete(&config_uri, 3, "entry: \"main.sbdsl\"\nout|");
    let output = items.iter().find(|item| item["label"] == "output").unwrap();
    assert!(
        output["textEdit"]["newText"]
            .as_str()
            .unwrap()
            .starts_with("output:")
    );
    assert!(!items.iter().any(|item| item["label"] == "entry"));
    client.stop();
}
