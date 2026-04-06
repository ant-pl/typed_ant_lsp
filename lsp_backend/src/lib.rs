mod utils;

use ant_ast::node::GetToken;
use ant_id::ModuleId;
use ant_lexer::Lexer;
use ant_name_resolver::NameResolver;
use ant_parser::Parser;
use ant_token::token::Token;
use ant_ty::{Ty, str_to_ty};
use ant_type_checker::TypeChecker;
use ant_type_checker::type_infer::TypeInfer;
use ant_type_checker::type_infer::infer_context::InferContext;
use ant_typed_ast::GetType;
use ant_typed_ast::typed_expr::TypedExpression;
use ant_typed_ast::typed_stmt::TypedStatement;
use ant_typed_module::module::TypedModule;
use ant_typed_module::ty_context::TypeContext;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::utils::UTF16Len;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticTokenTypeNumber {
    Namespace = 0,
    Type = 1,
    Class = 2,
    Enum = 3,
    Interface = 4,
    Struct = 5,
    TypeParameter = 6,
    Parameter = 7,
    Variable = 8,
    Property = 9,
    EnumMember = 10,
    Event = 11,
    Function = 12,
    Method = 13,
    Macro = 14,
    Keyword = 15,
    Modifier = 16,
    Comment = 17,
    String = 18,
    Number = 19,
    Regexp = 20,
    Operator = 21,
    Decorator = 22,
}

impl SemanticTokenTypeNumber {
    pub fn legend() -> Vec<SemanticTokenType> {
        vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::CLASS,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::STRUCT,
            SemanticTokenType::TYPE_PARAMETER,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::EVENT,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::MACRO,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::MODIFIER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::REGEXP,
            SemanticTokenType::OPERATOR,
            SemanticTokenType::DECORATOR,
        ]
    }
}

#[derive(Debug)]
pub struct AnalysisResult {
    pub tcx: TypeContext,
    pub typed_stmts: Vec<TypedStatement>,
    pub typed_exprs: Vec<TypedExpression>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct Backend(
    pub Client,
    pub Arc<RwLock<HashMap<Url, (String, Arc<AnalysisResult>)>>>,
);

fn get_lsp_range(text: &str, token: &Token) -> Range {
    let line_idx = (token.line.saturating_sub(1)) as usize;
    let line_text = match text.lines().nth(line_idx) {
        Some(l) => l,
        None => return Range::default(), // 行号超了直接跳过
    };

    let start_char = line_text
        .chars()
        .take(token.column.saturating_sub(1))
        .map(|c| c.len_utf16())
        .sum::<usize>() as u32;
    let end_char = start_char + token.value.chars().map(|c| c.len_utf16()).sum::<usize>() as u32;
    Range {
        start: Position {
            line: line_idx as u32,
            character: start_char,
        },
        end: Position {
            line: line_idx as u32,
            character: end_char,
        },
    }
}

fn current_ident(text: &str, position: Position) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if position.line as usize >= lines.len() {
        return String::new();
    }
    let line = lines[position.line as usize];
    let mut utf16_offset = 0usize;
    let mut char_idx = 0usize;
    for (i, c) in line.char_indices() {
        if utf16_offset >= position.character as usize {
            break;
        }
        utf16_offset += c.len_utf16();
        char_idx = i + c.len_utf8();
    }
    let before = &line[..char_idx.min(line.len())];
    before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

impl Backend {
    async fn run_analysis(&self, uri: &Url, text: &str) -> Arc<AnalysisResult> {
        let mut tcx = TypeContext::new();
        let mut module = TypedModule::new(&mut tcx);
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| std::path::PathBuf::from(uri.to_string()));
        let path_str: Arc<str> = path.to_string_lossy().to_string().into();

        let mut diagnostics = Vec::new();
        let tokens = Lexer::new(text.to_string(), path_str.clone()).get_tokens();

        let ast = match Parser::new(tokens).parse_program() {
            Ok(it) => it,
            Err(e) => {
                diagnostics.push(Diagnostic {
                    range: get_lsp_range(text, &e.token),
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: e.message.unwrap_or(e.kind.to_string().into()).to_string(),
                    ..Default::default()
                });
                return Arc::new(AnalysisResult {
                    tcx,
                    typed_stmts: vec![],
                    typed_exprs: vec![],
                    diagnostics,
                });
            }
        };

        let mut name_resolver = NameResolver::new(ModuleId(0), path_str);
        if let Err(e) = name_resolver.resolve(ast.clone()) {
            diagnostics.push(Diagnostic {
                range: get_lsp_range(text, &e.token),
                severity: Some(DiagnosticSeverity::ERROR),
                message: e.message.unwrap_or_default().to_string(),
                ..Default::default()
            });
            return Arc::new(AnalysisResult {
                tcx,
                typed_stmts: vec![],
                typed_exprs: vec![],
                diagnostics,
            });
        }

        let mut checker = TypeChecker::new(&mut module, &mut name_resolver);
        if let Err(e) = checker.check_all(ast) {
            diagnostics.push(Diagnostic {
                range: get_lsp_range(text, &e.token),
                severity: Some(DiagnosticSeverity::ERROR),
                message: e.message.unwrap_or_default().to_string(),
                ..Default::default()
            });

            return Arc::new(AnalysisResult {
                typed_stmts: module.typed_stmts,
                typed_exprs: module.typed_exprs,
                diagnostics,
                tcx,
            });
        }

        let constraints = checker.get_constraints().to_vec();
        let mut infer_ctx = InferContext::new(&mut module);
        let mut type_infer = TypeInfer::new(&mut infer_ctx, &mut name_resolver);

        if let Err(e) = type_infer.unify_all(constraints) {
            diagnostics.push(Diagnostic {
                range: get_lsp_range(text, &e.token),
                severity: Some(DiagnosticSeverity::ERROR),
                message: e.message.unwrap_or_default().to_string(),
                ..Default::default()
            });

            return Arc::new(AnalysisResult {
                typed_stmts: module.typed_stmts,
                typed_exprs: module.typed_exprs,
                diagnostics,
                tcx,
            });
        };
        
        
        if let Err(e) = type_infer.infer() {
            diagnostics.push(Diagnostic {
                range: get_lsp_range(text, &e.token),
                severity: Some(DiagnosticSeverity::ERROR),
                message: e.message.unwrap_or_default().to_string(),
                ..Default::default()
            });
        }
        
        Arc::new(AnalysisResult {
            typed_stmts: module.typed_stmts,
            typed_exprs: module.typed_exprs,
            diagnostics,
            tcx,
        })
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), ":".into()]),
                    ..Default::default()
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                        SemanticTokensRegistrationOptions {
                            text_document_registration_options: TextDocumentRegistrationOptions {
                                document_selector: Some(vec![DocumentFilter {
                                    language: Some("TypedAnt".into()),
                                    scheme: Some("file".into()),
                                    pattern: None,
                                }]),
                            },
                            semantic_tokens_options: SemanticTokensOptions {
                                legend: SemanticTokensLegend {
                                    token_types: SemanticTokenTypeNumber::legend(),
                                    token_modifiers: vec![],
                                },
                                full: Some(SemanticTokensFullOptions::Bool(true)),
                                ..Default::default()
                            },
                            static_registration_options: StaticRegistrationOptions::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "TypedAnt LSP".into(),
                version: Some("0.1.0".into()),
            }),
        })
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let res = self
            .run_analysis(&params.text_document.uri, &params.text_document.text)
            .await;
        self.0
            .publish_diagnostics(
                params.text_document.uri.clone(),
                res.diagnostics.clone(),
                None,
            )
            .await;
        self.1
            .write()
            .await
            .insert(params.text_document.uri, (params.text_document.text, res));
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.last() {
            let res = self
                .run_analysis(&params.text_document.uri, &change.text)
                .await;
            self.0
                .publish_diagnostics(
                    params.text_document.uri.clone(),
                    res.diagnostics.clone(),
                    None,
                )
                .await;
            self.1
                .write()
                .await
                .insert(params.text_document.uri, (change.text.clone(), res));
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let states = self.1.read().await;
        let (text, res) = match states.get(&params.text_document_position.text_document.uri) {
            Some(it) => it,
            None => return Ok(None),
        };
        let prefix = current_ident(text, params.text_document_position.position);
        let table = res.tcx.table.lock().unwrap();
        let items = table
            .var_map
            .iter()
            .filter(|(name, _)| name.starts_with(&prefix))
            .map(|(name, sym)| CompletionItem {
                label: name.to_string(),
                kind: Some(match res.tcx.get(sym.ty.get_type()) {
                    Ty::Function { .. } => CompletionItemKind::FUNCTION,
                    Ty::Struct { .. } => CompletionItemKind::STRUCT,
                    _ => CompletionItemKind::VARIABLE,
                }),
                ..Default::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let states = self.1.read().await;
        let (text, res) = match states.get(&uri) {
            Some(it) => it,
            None => return Ok(None),
        };

        let mut intermediate = Vec::new();

        let current_file_path = uri
            .to_file_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        for expr in &res.typed_exprs {
            if expr.token().file.as_ref() != current_file_path {
                continue;
            }

            match expr {
                TypedExpression::Ident(ident, ty_id, _) => {
                    let range = get_lsp_range(text, &ident.token);
                    let ty = res.tcx.get(*ty_id);
                    let token_type = if str_to_ty(&ident.value).is_some() {
                        SemanticTokenTypeNumber::Type
                    } else {
                        match ty {
                            Ty::Function { .. } => SemanticTokenTypeNumber::Function,
                            Ty::Generic(..) => SemanticTokenTypeNumber::TypeParameter,
                            _ => SemanticTokenTypeNumber::Variable,
                        }
                    };
                    intermediate.push((
                        range.start,
                        ident.token.value.utf16_len() as u32,
                        token_type,
                    ));
                }
                TypedExpression::Function { name: Some(tk), .. } => {
                    let range = get_lsp_range(text, tk);
                    intermediate.push((
                        range.start,
                        tk.value.utf16_len() as u32,
                        SemanticTokenTypeNumber::Function,
                    ));
                }
                TypedExpression::TypePath { left: ident, .. } => {
                    let range = get_lsp_range(text, &ident.token);
                    intermediate.push((
                        range.start,
                        ident.token.value.utf16_len() as u32,
                        SemanticTokenTypeNumber::Struct,
                    ));
                }
                _ => {}
            }
        }

        for stmt in &res.typed_stmts {
            if stmt.token().file.as_ref() != current_file_path {
                continue;
            }

            match stmt {
                TypedStatement::Use { full_path, .. } => {
                    for (i, tk) in full_path.iter().enumerate() {
                        let range = get_lsp_range(text, tk);
                        let ty = if i == full_path.len() - 1 {
                            SemanticTokenTypeNumber::Type
                        } else {
                            SemanticTokenTypeNumber::Namespace
                        };
                        intermediate.push((range.start, tk.value.utf16_len() as u32, ty));
                    }
                }
                TypedStatement::Struct { name, .. } => {
                    let range = get_lsp_range(text, &name.token);
                    intermediate.push((
                        range.start,
                        name.token.value.utf16_len() as u32,
                        SemanticTokenTypeNumber::Struct,
                    ));
                }
                TypedStatement::Extern { alias, .. } => {
                    let range = get_lsp_range(text, alias);
                    intermediate.push((
                        range.start,
                        alias.value.utf16_len() as u32,
                        SemanticTokenTypeNumber::Function,
                    ));
                }
                _ => {}
            }
        }

        intermediate.sort_by(|a, b| {
            a.0.line
                .cmp(&b.0.line)
                .then(a.0.character.cmp(&b.0.character))
        });
        intermediate.dedup_by(|a, b| a.0 == b.0);

        let mut data = Vec::new();
        let (mut last_line, mut last_start) = (0, 0);
        for (pos, len, ty) in intermediate {
            let delta_line = pos.line - last_line;
            let delta_start = if delta_line == 0 {
                pos.character - last_start
            } else {
                pos.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: len,
                token_type: ty as u32,
                token_modifiers_bitset: 0,
            });
            last_line = pos.line;
            last_start = pos.character;
        }
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
