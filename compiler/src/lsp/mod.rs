//! A stdio LSP endpoint. Analysis runs on immutable snapshots off the protocol thread.
pub mod completion;
pub mod symbols;
mod text;

use crate::{
    Error, Result,
    analysis::{self, AnalysisCache},
    registry::Registry,
};
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    CompletionParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Document {
    path: Option<PathBuf>,
    source: String,
    version: i32,
    language: String,
}

struct Snapshot {
    revision: u64,
    documents: BTreeMap<String, Document>,
    allow_extensions: bool,
}

struct AnalysisOutput {
    revision: u64,
    diagnostics: BTreeMap<String, Vec<Value>>,
    registries: BTreeMap<String, Registry>,
}

fn analyze(snapshot: Snapshot, cache: &mut AnalysisCache) -> AnalysisOutput {
    let overlays: BTreeMap<_, _> = snapshot
        .documents
        .values()
        .filter_map(|doc| {
            doc.path
                .as_ref()
                .map(|path| (path.clone(), doc.source.clone()))
        })
        .collect();
    let mut output = AnalysisOutput {
        revision: snapshot.revision,
        diagnostics: BTreeMap::new(),
        registries: BTreeMap::new(),
    };
    let paths = snapshot
        .documents
        .values()
        .filter(|doc| matches!(doc.language.as_str(), "sbdsl" | "sbdsl-build"))
        .filter_map(|doc| doc.path.clone())
        .collect::<Vec<_>>();
    let results = analysis::analyze_documents(&paths, &overlays, cache, snapshot.allow_extensions);
    for (uri, doc) in &snapshot.documents {
        if !matches!(doc.language.as_str(), "sbdsl" | "sbdsl-build") {
            continue;
        }
        output.diagnostics.entry(uri.clone()).or_default();
        if let Some(path) = &doc.path {
            let result = &results[&analysis::normalize_path(path)];
            output
                .registries
                .insert(uri.clone(), result.registry.clone());
            for diagnostic in &result.diagnostics {
                let path = analysis::normalize_path(&diagnostic.path);
                let opened = snapshot
                    .documents
                    .iter()
                    .find(|(_, doc)| doc.path.as_ref() == Some(&path));
                let Some(target_uri) = opened
                    .map(|(uri, _)| uri.clone())
                    .or_else(|| text::related_uri(&path, uri))
                    .or_else(|| text::path_uri(&path))
                else {
                    continue;
                };
                let source = opened
                    .map(|(_, doc)| doc.source.clone())
                    .unwrap_or_else(|| std::fs::read_to_string(&path).unwrap_or_default());
                let value = json!({
                    "range": {"start": text::position(&source, diagnostic.start), "end": text::position(&source, diagnostic.end)},
                    "severity": diagnostic.severity,
                    "source": "ScratchDSL", "message": diagnostic.message
                });
                let entries = output.diagnostics.entry(target_uri).or_default();
                if !entries.contains(&value) {
                    entries.push(value);
                }
            }
        } else {
            let errors = if doc.language == "sbdsl-build" {
                crate::notation::parse_build::<crate::config::Config>(
                    std::path::Path::new(uri),
                    &doc.source,
                )
                .err()
                .into_iter()
                .collect()
            } else {
                crate::parser::parse_recovering(std::path::Path::new(uri), &doc.source).errors
            };
            for error in errors {
                let (start, end) = error.span().map_or((0, 0), |span| (span.start, span.end));
                output.diagnostics.get_mut(uri).unwrap().push(json!({
                    "range": {"start": text::position(&doc.source, start), "end": text::position(&doc.source, end)},
                    "severity": 1, "source": "ScratchDSL", "message": error.0
                }));
            }
        }
    }
    output
}

fn send(connection: &Connection, message: impl Into<Message>) -> Result<()> {
    connection
        .sender
        .send(message.into())
        .map_err(|e| Error::new(e.to_string()))
}

fn publish(
    connection: &Connection,
    uri: &str,
    diagnostics: Vec<Value>,
    version: Option<i32>,
) -> Result<()> {
    let mut params = json!({"uri": uri, "diagnostics": diagnostics});
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    send(
        connection,
        Notification::new("textDocument/publishDiagnostics".into(), params),
    )
}

/// Do not write anything to stdout except framed JSON-RPC messages.
pub fn run_stdio() -> Result<()> {
    let (connection, io_threads) = Connection::stdio();
    let result = serve(&connection);
    drop(connection);
    // On a normal shutdown, the transport reader terminates at `exit` itself.
    // A malformed client may keep stdin open; do not block error termination.
    if result.is_ok() {
        io_threads.join()?;
    }
    result
}

fn serve(connection: &Connection) -> Result<()> {
    let (id, params) = connection
        .initialize_start()
        .map_err(|e| Error::new(e.to_string()))?;
    let allow_extensions = params
        .pointer("/initializationOptions/allowExtensions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    connection.initialize_finish(id, json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
            "completionProvider": {"resolveProvider": false, "triggerCharacters": ["[", "(", "<", "$", "@", ":", ".", "/", " "]}
        },
        "serverInfo": {"name": "ScratchDSL", "version": env!("CARGO_PKG_VERSION")}
    })).map_err(|e| Error::new(e.to_string()))?;

    let (jobs_tx, jobs_rx) = mpsc::channel::<Snapshot>();
    let (results_tx, results_rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("sbdsl-analysis".into())
        .spawn(move || {
            let mut cache = AnalysisCache::default();
            while let Ok(snapshot) = jobs_rx.recv() {
                if results_tx.send(analyze(snapshot, &mut cache)).is_err() {
                    break;
                }
            }
        })?;
    let result = event_loop(connection, &jobs_tx, &results_rx, allow_extensions);
    drop(jobs_tx);
    drop(results_rx);
    // CLI process exit must not wait for a getInfo() timeout in an obsolete job.
    // A detached worker cannot publish after the receiving channel is closed.
    if worker.is_finished() {
        worker
            .join()
            .map_err(|_| Error::new("语言分析线程异常退出"))?;
    }
    result
}

fn event_loop(
    connection: &Connection,
    jobs: &mpsc::Sender<Snapshot>,
    results: &mpsc::Receiver<AnalysisOutput>,
    allow_extensions: bool,
) -> Result<()> {
    let mut documents: BTreeMap<String, Document> = BTreeMap::new();
    let mut registries = BTreeMap::new();
    let builtin = Registry::new();
    let mut published = BTreeSet::new();
    let mut revision = 0;
    let mut pending = None;
    let mut working = false;
    let mut shutdown = false;
    loop {
        while let Ok(output) = results.try_recv() {
            working = false;
            if shutdown || output.revision != revision {
                continue;
            }
            for uri in published.difference(&output.diagnostics.keys().cloned().collect()) {
                publish(
                    connection,
                    uri,
                    vec![],
                    documents.get(uri).map(|d| d.version),
                )?;
            }
            published = output.diagnostics.keys().cloned().collect();
            for (uri, diagnostics) in output.diagnostics {
                publish(
                    connection,
                    &uri,
                    diagnostics,
                    documents.get(&uri).map(|d| d.version),
                )?;
            }
            registries = output.registries;
        }
        if !shutdown && !working && pending.is_some_and(|deadline| Instant::now() >= deadline) {
            jobs.send(Snapshot {
                revision,
                documents: documents.clone(),
                allow_extensions,
            })
            .map_err(|e| Error::new(e.to_string()))?;
            working = true;
            pending = None;
        }
        let message = match connection.receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(message) => message,
            Err(error) if error.is_disconnected() => return Ok(()),
            Err(_) => continue,
        };
        match message {
            Message::Request(request) => {
                if shutdown {
                    send(
                        connection,
                        Response::new_err(request.id, -32600, "服务器已关闭".into()),
                    )?;
                } else if request.method == "shutdown" {
                    send(connection, Response::new_ok(request.id, Value::Null))?;
                    shutdown = true;
                } else {
                    respond(connection, request, &documents, &registries, &builtin)?;
                }
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    return if shutdown {
                        Ok(())
                    } else {
                        Err(Error::new("客户端未先请求 shutdown"))
                    };
                }
                if shutdown {
                    continue;
                }
                let mut changed = false;
                match notification.method.as_str() {
                    "textDocument/didOpen" => {
                        if let Ok(params) =
                            serde_json::from_value::<DidOpenTextDocumentParams>(notification.params)
                        {
                            let doc = params.text_document;
                            let uri = doc.uri.as_str().to_owned();
                            if doc.language_id != "sbdsl" {
                                registries.clear();
                            }
                            documents.insert(
                                uri.clone(),
                                Document {
                                    path: text::uri_path(&uri),
                                    source: doc.text,
                                    version: doc.version,
                                    language: doc.language_id,
                                },
                            );
                            changed = true;
                        }
                    }
                    "textDocument/didChange" => {
                        if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(
                            notification.params,
                        ) && let Some(doc) = documents.get_mut(params.text_document.uri.as_str())
                            && params.text_document.version > doc.version
                            && let Some(change) = params.content_changes.last()
                            && change.range.is_none()
                        {
                            doc.source = change.text.clone();
                            doc.version = params.text_document.version;
                            if doc.language != "sbdsl" {
                                registries.clear();
                            }
                            changed = true;
                        }
                    }
                    "textDocument/didClose" => {
                        if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(
                            notification.params,
                        ) {
                            let uri = params.text_document.uri.as_str();
                            if documents
                                .remove(uri)
                                .is_some_and(|doc| doc.language != "sbdsl")
                            {
                                registries.clear();
                            }
                            registries.remove(uri);
                            published.remove(uri);
                            publish(connection, uri, vec![], None)?;
                            changed = true;
                        }
                    }
                    "workspace/didChangeWatchedFiles" | "textDocument/didSave" => {
                        registries.clear();
                        changed = true;
                    }
                    _ => {}
                }
                if changed {
                    revision += 1;
                    pending = Some(Instant::now() + Duration::from_millis(150));
                }
            }
            Message::Response(_) => {}
        }
    }
}

fn respond(
    connection: &Connection,
    request: Request,
    documents: &BTreeMap<String, Document>,
    registries: &BTreeMap<String, Registry>,
    builtin: &Registry,
) -> Result<()> {
    if request.method != "textDocument/completion" {
        return send(
            connection,
            Response::new_err(
                request.id,
                -32601,
                format!("不支持的方法 {}", request.method),
            ),
        );
    }
    let params: CompletionParams = match serde_json::from_value(request.params) {
        Ok(params) => params,
        Err(error) => {
            return send(
                connection,
                Response::new_err(request.id, -32602, error.to_string()),
            );
        }
    };
    let uri = params.text_document_position.text_document.uri.as_str();
    let items = documents
        .get(uri)
        .filter(|doc| matches!(doc.language.as_str(), "sbdsl" | "sbdsl-build"))
        .map_or_else(Vec::new, |doc| {
            let position = params.text_document_position.position;
            if doc.language == "sbdsl-build" {
                return completion::complete_config(&doc.source, position);
            }
            let Some(cursor) = completion::byte_offset(&doc.source, position) else {
                return Vec::new();
            };
            let overlays = documents
                .values()
                .filter_map(|document| {
                    document
                        .path
                        .as_ref()
                        .map(|path| (path.clone(), document.source.clone()))
                })
                .collect();
            let symbols = symbols::collect(doc.path.as_deref(), &doc.source, &overlays, cursor);
            completion::complete_with_symbols(
                &doc.source,
                position,
                registries.get(uri).unwrap_or(builtin),
                &symbols,
            )
        });
    send(
        connection,
        Response::new_ok(request.id, json!({"isIncomplete": true, "items": items})),
    )
}
