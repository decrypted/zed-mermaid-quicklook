//! Mermaid Quicklook language server.
//!
//! Renders a `.mmd` file to a PNG in a temp directory and asks Zed to open it as an
//! image tab. Zed watches that file, so re-rendering on save updates the open tab in
//! place: a live preview. Source files are never modified.

mod render;

use anyhow::{anyhow, Context, Result};
use crossbeam_channel::Sender;
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability,
    Command as LspCommand, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, ExecuteCommandOptions, ExecuteCommandParams, MessageType,
    ServerCapabilities, ShowMessageParams, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Url, WorkDoneProgressOptions,
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    env,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const CMD_PREVIEW: &str = "mermaid.preview";
const CMD_EXPORT_PNG: &str = "mermaid.exportPng";
const CMD_EXPORT_SVG: &str = "mermaid.exportSvg";

/// Documents we've opened a preview tab for. We cannot observe the preview tab's
/// lifecycle (an image tab is not a text document, so no didOpen/didClose reaches us),
/// so we simply keep re-rendering on save while the *source* document is open. If the
/// user closed the preview, the cost is one wasted render.
#[derive(Default)]
struct State {
    documents: HashMap<String, String>,
    previewing: HashSet<String>,
}

fn main() -> Result<()> {
    eprintln!("mermaid-quicklook-lsp starting");

    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        // Diagrams are small; full sync keeps the server trivially correct.
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::FULL),
            save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            ..Default::default()
        })),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                CMD_PREVIEW.to_string(),
                CMD_EXPORT_PNG.to_string(),
                CMD_EXPORT_SVG.to_string(),
            ],
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        ..Default::default()
    })?;

    connection.initialize(capabilities)?;
    eprintln!("initialized");

    let mut state = State::default();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle_request(&connection, &mut state, request);
            }
            Message::Notification(notification) => {
                handle_notification(&connection, &mut state, notification);
            }
            Message::Response(_) => {}
        }
    }

    io_threads.join()?;
    Ok(())
}

fn handle_request(connection: &Connection, state: &mut State, request: Request) {
    let id = request.id.clone();

    let result = match request.method.as_str() {
        "textDocument/codeAction" => cast::<CodeActionParams>(request)
            .map(|params| json!(code_actions(&params))),
        "workspace/executeCommand" => cast::<ExecuteCommandParams>(request).map(|params| {
            if let Err(error) = execute_command(connection, state, &params) {
                show_error(&connection.sender, &error.to_string());
            }
            json!(null)
        }),
        _ => Ok(json!(null)),
    };

    let response = match result {
        Ok(value) => Response::new_ok(id, value),
        Err(error) => {
            eprintln!("bad request: {error}");
            Response::new_ok(id, json!(null))
        }
    };

    let _ = connection.sender.send(Message::Response(response));
}

fn cast<P: serde::de::DeserializeOwned>(request: Request) -> Result<P> {
    serde_json::from_value(request.params).map_err(|error| anyhow!("{error}"))
}

fn handle_notification(connection: &Connection, state: &mut State, notification: Notification) {
    match notification.method.as_str() {
        "textDocument/didOpen" => {
            if let Ok(params) = serde_json::from_value::<DidOpenTextDocumentParams>(notification.params) {
                state.documents.insert(
                    params.text_document.uri.to_string(),
                    params.text_document.text,
                );
            }
        }
        "textDocument/didChange" => {
            // Full sync: the last change carries the whole document.
            if let Ok(params) = serde_json::from_value::<serde_json::Value>(notification.params) {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                if let Some(text) = params["contentChanges"]
                    .as_array()
                    .and_then(|changes| changes.last())
                    .and_then(|change| change["text"].as_str())
                {
                    state.documents.insert(uri.to_string(), text.to_string());
                }
            }
        }
        "textDocument/didSave" => {
            if let Ok(params) = serde_json::from_value::<DidSaveTextDocumentParams>(notification.params) {
                let uri = params.text_document.uri.to_string();
                if state.previewing.contains(&uri) {
                    if let Some(source) = state.documents.get(&uri).cloned() {
                        // Overwrite the same file the preview tab is showing; Zed repaints it.
                        refresh_preview(&connection.sender, uri, source);
                    }
                }
            }
        }
        "textDocument/didClose" => {
            if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(notification.params) {
                let uri = params.text_document.uri.to_string();
                state.documents.remove(&uri);
                state.previewing.remove(&uri);
                if let Ok(path) = preview_path(&uri) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        _ => {}
    }
}

fn code_actions(params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    let uri = params.text_document.uri.to_string();
    if !is_mermaid_document(&uri) {
        return Vec::new();
    }

    let argument = json!({ "uri": uri });

    [
        ("Open Mermaid Preview", CMD_PREVIEW),
        ("Export Mermaid Diagram as PNG", CMD_EXPORT_PNG),
        ("Export Mermaid Diagram as SVG", CMD_EXPORT_SVG),
    ]
    .into_iter()
    .map(|(title, command)| {
        CodeActionOrCommand::CodeAction(CodeAction {
            title: title.to_string(),
            kind: Some(CodeActionKind::EMPTY),
            command: Some(LspCommand {
                title: title.to_string(),
                command: command.to_string(),
                arguments: Some(vec![argument.clone()]),
            }),
            ..Default::default()
        })
    })
    .collect()
}

fn execute_command(
    connection: &Connection,
    state: &mut State,
    params: &ExecuteCommandParams,
) -> Result<()> {
    let uri = params
        .arguments
        .first()
        .and_then(|argument| argument.get("uri"))
        .and_then(|uri| uri.as_str())
        .ok_or_else(|| anyhow!("command is missing its uri argument"))?
        .to_string();

    let source = state
        .documents
        .get(&uri)
        .cloned()
        .ok_or_else(|| anyhow!("document is not open: {uri}"))?;

    match params.command.as_str() {
        CMD_PREVIEW => {
            let target = preview_path(&uri)?;
            render::render(&source, &target)?;
            open_in_editor(&target)?;
            state.previewing.insert(uri);
            Ok(())
        }
        CMD_EXPORT_PNG => export(connection, &uri, &source, "png"),
        CMD_EXPORT_SVG => export(connection, &uri, &source, "svg"),
        other => Err(anyhow!("unknown command: {other}")),
    }
}

fn export(connection: &Connection, uri: &str, source: &str, format: &str) -> Result<()> {
    let target = source_path(uri)?.with_extension(format);
    render::render(source, &target)?;
    show_info(
        &connection.sender,
        &format!("Exported {}", target.display()),
    );
    Ok(())
}

/// Re-render an open preview off the main loop. mmdc boots Chromium and takes a second
/// or two; blocking here would stall every other LSP request behind a save.
fn refresh_preview(sender: &Sender<Message>, uri: String, source: String) {
    let sender = sender.clone();
    std::thread::spawn(move || {
        let target = match preview_path(&uri) {
            Ok(target) => target,
            Err(error) => {
                show_error(&sender, &error.to_string());
                return;
            }
        };

        if let Err(error) = render::render(&source, &target) {
            // A syntax error mid-edit is normal and expected. Report it, but leave the
            // last good render in the tab rather than blanking it.
            show_error(&sender, &format!("Mermaid: {error}"));
        }
    });
}

/// Ask the running Zed instance to open the rendered image as a tab.
///
/// Zed does not implement the LSP `window/showDocument` request for regular language
/// servers (only Copilot registers a handler, and it is a no-op stub), so we go through
/// the `zed` CLI instead. Re-opening an already-open path just focuses that tab.
fn open_in_editor(path: &Path) -> Result<()> {
    for binary in ["zed", "zeditor", "xdg-open"] {
        let spawned = Command::new(binary)
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        if spawned.is_ok() {
            return Ok(());
        }
    }

    Err(anyhow!(
        "could not open {}: no `zed` CLI on PATH",
        path.display()
    ))
}

fn is_mermaid_document(uri: &str) -> bool {
    uri.ends_with(".mmd") || uri.ends_with(".mermaid")
}

fn source_path(uri: &str) -> Result<PathBuf> {
    Url::parse(uri)
        .context("parsing document uri")?
        .to_file_path()
        .map_err(|_| anyhow!("document is not a local file: {uri}"))
}

/// Stable per-document path, so re-rendering overwrites the file the preview tab holds
/// open. The basename is the source's, so the tab reads `diagram.png` rather than a hash.
fn preview_path(uri: &str) -> Result<PathBuf> {
    let source = source_path(uri)?;
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("diagram");

    let mut hasher = DefaultHasher::new();
    uri.hash(&mut hasher);

    Ok(env::temp_dir()
        .join("zed-mermaid-quicklook")
        .join(format!("{:016x}", hasher.finish()))
        .join(format!("{stem}.png")))
}

fn show_error(sender: &Sender<Message>, message: &str) {
    notify(sender, MessageType::ERROR, message);
}

fn show_info(sender: &Sender<Message>, message: &str) {
    notify(sender, MessageType::INFO, message);
}

fn notify(sender: &Sender<Message>, typ: MessageType, message: &str) {
    let params = ShowMessageParams {
        typ,
        message: message.to_string(),
    };

    let _ = sender.send(Message::Notification(Notification {
        method: "window/showMessage".to_string(),
        params: json!(params),
    }));
}
