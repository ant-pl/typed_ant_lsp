mod utils;

use ant_lexer::Lexer;
use ant_parser::Parser;
use ant_token::token::Token;
use ant_type_checker::{TypeChecker, type_infer};
use ant_type_checker::module::TypedModule;
use ant_type_checker::ty::Ty;
use ant_type_checker::ty_context::TypeContext;
use ant_type_checker::type_infer::TypeInfer;
use ant_type_checker::type_infer::infer_context::InferContext;
use ant_type_checker::typed_ast::GetType;

use std::collections::HashMap;
use tokio::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::utils::UTF16Len;

/* =========================
 * Backend
 * ========================= */

#[derive(Debug)]
pub struct Backend {
    pub client: Client,
    pub documents: RwLock<HashMap<Url, String>>,
}

/* =========================
 * Utils
 * ========================= */

/// 获取光标前的标识符（UTF-8 / UTF-16 安全）
fn current_ident(text: &str, position: Position) -> String {
    let mut line_start = 0usize;
    let mut current_line = 0u32;

    for (i, c) in text.char_indices() {
        if current_line == position.line {
            break;
        }
        if c == '\n' {
            current_line += 1;
            line_start = i + 1;
        }
    }

    let line = &text[line_start..];
    let mut col_bytes = 0usize;
    let mut chars = line.chars();

    for _ in 0..position.character {
        if let Some(c) = chars.next() {
            col_bytes += c.len_utf8();
        }
    }

    let before = &line[..col_bytes.min(line.len())];

    before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Token → LSP range（UTF-16）
fn calc_token_pos(text: &str, token: &Token) -> (u32, u32) {
    let line_text = text.lines().nth(token.line - 1).unwrap_or("");
    let prefix: String = line_text.chars().take(token.column - 1).collect();

    let start = prefix.utf16_len() as u32;
    let end = start + token.value.utf16_len() as u32;

    (start, end)
}

/* =========================
 * Core analyze (不碰 client)
 * ========================= */

fn analyze(
    text: &str,
    uri: &Url,

    // 各种上下文
    module: &mut TypedModule,
) -> std::result::Result<(), Diagnostic> {
    let file = uri
        .to_file_path()
        .map_or(uri.to_string(), |it| it.to_string_lossy().to_string());

    /* ---------- lexer ---------- */
    let mut lexer = Lexer::new(text.to_string(), file.clone().into());
    let tokens = lexer.get_tokens();

    if lexer.contains_error() {
        return Err(Diagnostic {
            severity: Some(DiagnosticSeverity::ERROR),
            message: "lexer error".into(),
            source: Some(file),
            ..Default::default()
        });
    }

    /* ---------- parser ---------- */
    let mut parser = Parser::new(tokens);
    let ast = parser.parse_program().map_err(|err| {
        let line = (err.token.line - 1) as u32;
        let (start, end) = calc_token_pos(text, &err.token);

        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: start,
                },
                end: Position {
                    line,
                    character: end,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            message: err
                .message
                .unwrap_or(err.kind.to_string().into())
                .to_string(),
            source: Some(file.clone()),
            ..Default::default()
        }
    })?;

    /* ---------- type checker ---------- */
    let mut checker = TypeChecker::new(module);

    checker.check_node(ast).map_err(|err| {
        let line = (err.token.line - 1) as u32;
        let (start, end) = calc_token_pos(text, &err.token);

        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: start,
                },
                end: Position {
                    line,
                    character: end,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            message: err
                .message
                .unwrap_or(err.kind.to_string().into())
                .to_string(),
            source: Some(file.clone()),
            ..Default::default()
        }
    })?;

    let mut infer_ctx = InferContext::new(module);
    let mut type_infer = TypeInfer::new(&mut infer_ctx);

    type_infer.infer().map_err(|err| {
        let line = (err.token.line - 1) as u32;
        let (start, end) = calc_token_pos(text, &err.token);

        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: start,
                },
                end: Position {
                    line,
                    character: end,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            message: err
                .message
                .unwrap_or(err.kind.to_string().into())
                .to_string(),
            source: Some(file),
            ..Default::default()
        }
    })?;

    Ok(())
}

/* =========================
 * 文档事件专用：publish diagnostics
 * ========================= */

async fn check_and_publish(client: &Client, uri: &Url, text: &str) -> Option<TypeContext> {
    let mut tcx = TypeContext::new();

    let mut module = TypedModule::new(&mut tcx);

    match analyze(text, uri, &mut module) {
        Ok(_) => {
            client.publish_diagnostics(uri.clone(), vec![], None).await;
            Some(tcx)
        }
        Err(diag) => {
            client
                .publish_diagnostics(uri.clone(), vec![diag], None)
                .await;
            None
        }
    }
}

/* =========================
 * LSP impl
 * ========================= */

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["_".into()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "TypedAnt LSP".into(),
                version: Some("0.1.0".into()),
            }),
        })
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        self.documents
            .write()
            .await
            .insert(uri.clone(), text.clone());
        check_and_publish(&self.client, &uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;

        if let Some(change) = params.content_changes.last() {
            let text = change.text.clone();
            self.documents
                .write()
                .await
                .insert(uri.clone(), text.clone());
            check_and_publish(&self.client, &uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let docs = self.documents.read().await;
        let text = match docs.get(&uri) {
            Some(it) => it,
            None => return Ok(None),
        };

        let mut tcx = TypeContext::new();
        let mut module = TypedModule::new(&mut tcx);
        let _err = analyze(text, &uri, &mut module);

        let prefix = current_ident(text, pos);

        let items = tcx
            .table
            .lock()
            .unwrap()
            .var_map
            .iter()
            .filter(|(name, _)| name.starts_with(&prefix))
            .map(|(name, sym)| CompletionItem {
                label: name.to_string(),
                kind: Some(
                    match tcx.get(sym.ty.get_type()) {
                        Ty::Function { .. } => CompletionItemKind::FUNCTION,
                        Ty::Struct { .. } => CompletionItemKind::STRUCT,
                        _ => CompletionItemKind::VARIABLE
                    }
                ),
                insert_text: Some(name.to_string()),
                ..Default::default()
            })
            .collect();

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
