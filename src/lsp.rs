use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ieum::analysis::{self, Analysis, Builtin, Category, FormUse, Symbol, SymbolKind};
use ieum::{SourceFiles, Span};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    Notification as NotificationKind, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest, Formatting,
    GotoDefinition, HoverRequest, PrepareRenameRequest, References, Rename, Request as RequestKind,
    SemanticTokensFullRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentChanges, DocumentFormattingParams, DocumentHighlight,
    DocumentHighlightParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    Documentation, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeResult, InsertTextFormat, Location, MarkupContent,
    MarkupKind, NumberOrString, OneOf, OptionalVersionedTextDocumentIdentifier, Position,
    PrepareRenameResponse, PublishDiagnosticsParams, Range, ReferenceParams, RenameOptions,
    RenameParams, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, ServerCapabilities, ServerInfo, TextDocumentEdit,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri,
    WorkDoneProgressOptions, WorkspaceEdit,
};

const QUIET: Duration = Duration::from_millis(300);

const TOKEN_TYPES: [SemanticTokenType; 7] = [
    SemanticTokenType::FUNCTION,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::TYPE,
    SemanticTokenType::KEYWORD,
    SemanticTokenType::new("control"),
];
const TOKEN_MODIFIERS: [SemanticTokenModifier; 3] = [
    SemanticTokenModifier::DECLARATION,
    SemanticTokenModifier::READONLY,
    SemanticTokenModifier::DEFAULT_LIBRARY,
];
const FUNCTION: u32 = 0;
const VARIABLE: u32 = 1;
const PARAMETER: u32 = 2;
const PROPERTY: u32 = 3;
const TYPE: u32 = 4;
const KEYWORD: u32 = 5;
const CONTROL: u32 = 6;
const DECLARATION: u32 = 1;
const READONLY: u32 = 1 << 1;
const DEFAULT_LIBRARY: u32 = 1 << 2;

pub(crate) fn serve() -> ExitCode {
    let (connection, threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_formatting_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![" ".to_owned()]),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            ..CodeActionOptions::default()
        })),
        semantic_tokens_provider: Some(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: TOKEN_TYPES.to_vec(),
                    token_modifiers: TOKEN_MODIFIERS.to_vec(),
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            }
            .into(),
        ),
        ..ServerCapabilities::default()
    };
    let result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: "ieum".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    let result = serde_json::to_value(result).expect("서버 기능은 JSON으로 바꿀 수 있습니다");
    let Ok((id, initialized)) = connection.initialize_start() else {
        return ExitCode::from(1);
    };
    if connection.initialize_finish(id, result).is_err() {
        return ExitCode::from(1);
    }
    let versioned_edits = initialized
        .pointer("/capabilities/workspace/workspaceEdit/documentChanges")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let refresh_tokens = initialized
        .pointer("/capabilities/workspace/semanticTokens/refreshSupport")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut server = Server {
        connection: &connection,
        documents: HashMap::new(),
        due: HashMap::new(),
        builtins: analysis::builtins(),
        sources: SourceFiles::default(),
        versioned_edits,
        refresh_tokens,
        refresh_pending: false,
        next_request: 0,
    };
    let served = server.run();
    drop(connection);
    match (served, threads.join()) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

struct Document {
    text: Rc<str>,
    version: i32,
    analysis: Option<Rc<Analysis>>,
    dependencies: Option<Vec<PathBuf>>,
}

struct Server<'a> {
    connection: &'a Connection,
    documents: HashMap<Uri, Document>,
    due: HashMap<Uri, Instant>,
    builtins: Vec<Builtin>,
    sources: SourceFiles,
    versioned_edits: bool,
    refresh_tokens: bool,
    refresh_pending: bool,
    next_request: i32,
}

type Served = Result<(), Box<dyn std::error::Error>>;

impl Server<'_> {
    fn run(&mut self) -> Served {
        loop {
            let wait = self
                .due
                .values()
                .min()
                .map(|when| when.saturating_duration_since(Instant::now()));
            let message = match wait {
                Some(wait) => match self.connection.receiver.recv_timeout(wait) {
                    Ok(message) => Some(message),
                    Err(error) if error.is_timeout() => None,
                    Err(_) => return Ok(()),
                },
                None => match self.connection.receiver.recv() {
                    Ok(message) => Some(message),
                    Err(_) => return Ok(()),
                },
            };
            match message {
                Some(Message::Request(request)) => {
                    if self.connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    self.request(request)?;
                }
                Some(Message::Notification(notification)) => self.notification(notification)?,
                Some(Message::Response(_)) => {}
                None => {}
            }
            self.publish_due()?;
        }
    }

    fn publish_due(&mut self) -> Served {
        let now = Instant::now();
        let ready: Vec<Uri> = self
            .due
            .iter()
            .filter(|(_, when)| **when <= now)
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in ready {
            self.due.remove(&uri);
            self.publish(&uri)?;
        }
        if self.refresh_pending && self.due.is_empty() {
            self.refresh_pending = false;
            if self.refresh_tokens {
                self.next_request = self.next_request.wrapping_add(1);
                self.connection.sender.send(Message::Request(Request::new(
                    RequestId::from(format!("ieum-{}", self.next_request)),
                    "workspace/semanticTokens/refresh".to_owned(),
                    (),
                )))?;
            }
        }
        Ok(())
    }

    fn request(&mut self, request: Request) -> Served {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            Formatting::METHOD => self.answer(request, |s, p: DocumentFormattingParams| {
                s.text(&p.text_document.uri).and_then(formatted)
            }),
            HoverRequest::METHOD => self.answer(request, |s, p: HoverParams| {
                let place = p.text_document_position_params;
                s.hover(&place.text_document.uri, place.position)
            }),
            Completion::METHOD => self.answer(request, |s, p: CompletionParams| {
                let spaced = p
                    .context
                    .and_then(|context| context.trigger_character)
                    .is_some_and(|typed| typed == " ");
                let place = p.text_document_position;
                s.completion(&place.text_document.uri, place.position, spaced)
            }),
            GotoDefinition::METHOD => self.answer(request, |s, p: GotoDefinitionParams| {
                let place = p.text_document_position_params;
                s.definition(&place.text_document.uri, place.position)
                    .map(GotoDefinitionResponse::Scalar)
            }),
            References::METHOD => self.answer(request, |s, p: ReferenceParams| {
                let place = p.text_document_position;
                let uri = place.text_document.uri;
                let with_definition = p.context.include_declaration;
                s.occurrences(&uri, place.position, with_definition)
                    .map(|spans| {
                        let text = s.text(&uri).unwrap_or_default();
                        spans
                            .into_iter()
                            .map(|span| Location::new(uri.clone(), range(text, span)))
                            .collect::<Vec<_>>()
                    })
            }),
            DocumentHighlightRequest::METHOD => {
                self.answer(request, |s, p: DocumentHighlightParams| {
                    let place = p.text_document_position_params;
                    let uri = place.text_document.uri;
                    s.occurrences(&uri, place.position, true).map(|spans| {
                        let text = s.text(&uri).unwrap_or_default();
                        spans
                            .into_iter()
                            .map(|span| DocumentHighlight {
                                range: range(text, span),
                                kind: None,
                            })
                            .collect::<Vec<_>>()
                    })
                })
            }
            DocumentSymbolRequest::METHOD => self.answer(request, |s, p: DocumentSymbolParams| {
                s.symbols(&p.text_document.uri)
                    .map(DocumentSymbolResponse::Nested)
            }),
            SemanticTokensFullRequest::METHOD => {
                self.answer(request, |s, p: SemanticTokensParams| {
                    s.tokens(&p.text_document.uri).map(|data| {
                        SemanticTokensResult::Tokens(SemanticTokens {
                            result_id: None,
                            data,
                        })
                    })
                })
            }
            PrepareRenameRequest::METHOD => {
                self.answer(request, |s, p: TextDocumentPositionParams| {
                    s.prepare_rename(&p.text_document.uri, p.position)
                })
            }
            Rename::METHOD => {
                let params = serde_json::from_value::<RenameParams>(request.params);
                match params.map(|p| {
                    let place = p.text_document_position;
                    self.rename(&place.text_document.uri, place.position, &p.new_name)
                }) {
                    Ok(Ok(edit)) => Response::new_ok(id, edit),
                    Ok(Err(message)) => {
                        Response::new_err(id, ErrorCode::RequestFailed as i32, message)
                    }
                    Err(error) => {
                        Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string())
                    }
                }
            }
            CodeActionRequest::METHOD => self.answer(request, |s, p: CodeActionParams| {
                Some(s.code_actions(&p.text_document.uri, &p.context.diagnostics))
            }),
            _ => Response::new_err(
                id,
                ErrorCode::MethodNotFound as i32,
                format!("지원하지 않는 요청입니다: {}", request.method),
            ),
        };
        self.connection.sender.send(Message::Response(result))?;
        Ok(())
    }

    fn answer<P: serde::de::DeserializeOwned, R: serde::Serialize>(
        &mut self,
        request: Request,
        handle: impl FnOnce(&mut Self, P) -> Option<R>,
    ) -> Response {
        let id: RequestId = request.id;
        match serde_json::from_value::<P>(request.params) {
            Ok(params) => Response::new_ok(id, handle(self, params)),
            Err(error) => Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()),
        }
    }

    fn notification(&mut self, notification: Notification) -> Served {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<DidOpenTextDocumentParams>(notification.params)
                else {
                    return Ok(());
                };
                let document = params.text_document;
                self.store(document.uri.clone(), document.text, document.version);
                self.due.remove(&document.uri);
                self.publish(&document.uri)?;
            }
            DidChangeTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<DidChangeTextDocumentParams>(notification.params)
                else {
                    return Ok(());
                };
                if let Some(change) = params.content_changes.into_iter().last() {
                    let uri = params.text_document.uri;
                    if self
                        .documents
                        .get(&uri)
                        .is_none_or(|document| document.version >= params.text_document.version)
                    {
                        return Ok(());
                    }
                    self.store(uri, change.text, params.text_document.version);
                }
            }
            DidCloseTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<DidCloseTextDocumentParams>(notification.params)
                else {
                    return Ok(());
                };
                let uri = params.text_document.uri;
                let path = path_of(&uri);
                self.documents.remove(&uri);
                if let Some(path) = &path {
                    self.sources.remove(path);
                }
                self.due.remove(&uri);
                self.invalidate(path.as_deref());
                self.send_diagnostics(uri, Vec::new(), None)?;
            }
            DidChangeWatchedFiles::METHOD => {
                if let Ok(params) =
                    serde_json::from_value::<DidChangeWatchedFilesParams>(notification.params)
                {
                    for change in params.changes {
                        self.invalidate(path_of(&change.uri).as_deref());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn store(&mut self, uri: Uri, text: String, version: i32) {
        let path = path_of(&uri);
        if let Some(path) = &path {
            self.sources.insert(path, text.as_bytes());
        }
        self.documents.insert(
            uri,
            Document {
                text: text.into(),
                version,
                analysis: None,
                dependencies: None,
            },
        );
        self.invalidate(path.as_deref());
    }

    fn text(&self, uri: &Uri) -> Option<&str> {
        self.documents.get(uri).map(|document| &*document.text)
    }

    fn invalidate(&mut self, path: Option<&Path>) {
        self.refresh_pending = true;
        let when = Instant::now() + QUIET;
        let path = path.map(SourceFiles::canonical_path);
        for (uri, document) in &mut self.documents {
            let affected = path.as_ref().is_none_or(|path| {
                path_of(uri)
                    .map(|own| SourceFiles::canonical_path(&own))
                    .as_ref()
                    == Some(path)
                    || document
                        .dependencies
                        .as_ref()
                        .is_none_or(|dependencies| dependencies.contains(path))
            });
            if !affected {
                continue;
            }
            document.analysis = None;
            self.due.insert(uri.clone(), when);
        }
    }

    fn analysis(&mut self, uri: &Uri) -> Option<(Rc<Analysis>, Rc<str>)> {
        let document = self.documents.get_mut(uri)?;
        if document.analysis.is_none() {
            let path = path_of(uri);
            let analysis =
                analysis::analyze(path.as_deref(), document.text.as_bytes(), &self.sources);
            document.dependencies = Some(analysis.dependencies.clone());
            document.analysis = Some(Rc::new(analysis));
        }
        let analysis = document.analysis.clone()?;
        Some((analysis, document.text.clone()))
    }

    fn lookup(&mut self, uri: &Uri, position: Position) -> Option<(Rc<Analysis>, Rc<str>, Found)> {
        let (analysis, text) = self.analysis(uri)?;
        let found = find(&analysis, &text, offset(&text, position))?;
        Some((analysis, text, found))
    }

    fn publish(&mut self, uri: &Uri) -> Served {
        self.analysis(uri);
        let Some(document) = self.documents.get(uri) else {
            return Ok(());
        };
        let found = diagnostics(uri, &document.text, &self.sources);
        self.send_diagnostics(uri.clone(), found, Some(document.version))
    }

    fn send_diagnostics(
        &self,
        uri: Uri,
        diagnostics: Vec<Diagnostic>,
        version: Option<i32>,
    ) -> Served {
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        };
        let notification = Notification::new(PublishDiagnostics::METHOD.to_owned(), params);
        self.connection
            .sender
            .send(Message::Notification(notification))?;
        Ok(())
    }

    fn hover(&mut self, uri: &Uri, position: Position) -> Option<Hover> {
        let (analysis, text, found) = self.lookup(uri, position)?;
        let text = &*text;
        let mut lines = Vec::new();
        match found.target {
            Target::Defined(definition) => {
                let symbol = analysis.symbol(definition)?;
                lines.push(format!(
                    "```ieum\n{}\n```",
                    line_of(text, symbol.span.start)
                ));
                let mut what = kind_name(symbol.kind).to_owned();
                if let Some(lemma) = symbol
                    .lemmas
                    .first()
                    .filter(|_| symbol.kind == SymbolKind::Function)
                {
                    what = format!("{what} `{lemma}`");
                }
                if let Some(module) = &symbol.imported_from {
                    what = format!("{what}, {}에서 가져옴", module.display());
                }
                lines.push(what);
            }
            Target::Builtin(ref name) => {
                let builtin = self.builtins.iter().find(|b| b.name == name)?;
                match &builtin.arguments {
                    Some(arguments) => lines.push(format!(
                        "**{}** 내장 동사\n\n인자: {arguments}",
                        builtin.name
                    )),
                    None => lines.push(format!("**{}** 내장 상수", builtin.name)),
                }
                lines.push(builtin.description.clone());
            }
        }
        let written = &text[found.word.start..found.word.end];
        if written != found.name && !written.trim_matches('`').eq(&found.name) {
            lines.push(format!(
                "`{written}`{} `{}`의 활용형입니다.",
                analysis::particle_after(written.trim_matches('`'), "는"),
                found.name
            ));
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: lines.join("\n\n"),
            }),
            range: Some(range(text, found.word)),
        })
    }

    fn definition(&mut self, uri: &Uri, position: Position) -> Option<Location> {
        let (analysis, text, found) = self.lookup(uri, position)?;
        let text = &*text;
        let Target::Defined(definition) = found.target else {
            return None;
        };
        let symbol = analysis.symbol(definition)?;
        if let Some(module) = &symbol.imported_from {
            let source = self.sources.read(module).ok()?;
            let span = analysis::declaration_span(&source, &symbol.name)?;
            let other = String::from_utf8_lossy(&source);
            let target = uri_of(module)?;
            return Some(Location::new(
                target,
                range(&other, word_span(&other, span)),
            ));
        }
        Some(Location::new(
            uri.clone(),
            range(text, word_span(text, symbol.span)),
        ))
    }

    fn occurrences(
        &mut self,
        uri: &Uri,
        position: Position,
        with_definition: bool,
    ) -> Option<Vec<Span>> {
        let (analysis, text, found) = self.lookup(uri, position)?;
        let text = &*text;
        let mut spans = Vec::new();
        match &found.target {
            Target::Defined(definition) => {
                if with_definition {
                    spans.push(word_span(text, *definition));
                }
                spans.extend(
                    analysis
                        .references
                        .iter()
                        .filter(|r| r.definition == Some(*definition))
                        .map(|r| word_span(text, r.span)),
                );
            }
            Target::Builtin(name) => spans.extend(
                analysis
                    .references
                    .iter()
                    .filter(|r| r.definition.is_none() && &r.name == name)
                    .map(|r| word_span(text, r.span)),
            ),
        }
        let mut seen = HashSet::new();
        spans.retain(|span| seen.insert(span.start));
        Some(spans)
    }

    fn symbols(&mut self, uri: &Uri) -> Option<Vec<DocumentSymbol>> {
        let (analysis, text) = self.analysis(uri)?;
        let text = &*text;
        let mut top = Vec::new();
        for symbol in analysis.symbols.iter().filter(|s| s.scope.is_none()) {
            let selection = range(text, word_span(text, symbol.span));
            let line = selection.start.line as usize;
            let end = block_end(text, line);
            let whole = Range::new(Position::new(line as u32, 0), position(text, end));
            let children = (symbol.kind == SymbolKind::Function).then(|| {
                analysis
                    .symbols
                    .iter()
                    .filter(|inner| {
                        inner.kind != SymbolKind::Parameter
                            && inner.scope.is_some()
                            && inner.span.start > symbol.span.start
                            && inner.span.end <= end
                    })
                    .map(|inner| leaf(text, inner))
                    .collect()
            });
            #[allow(deprecated)]
            top.push(DocumentSymbol {
                name: symbol.name.clone(),
                detail: Some(line_of(text, symbol.span.start).trim().to_owned()),
                kind: lsp_kind(symbol.kind),
                tags: None,
                deprecated: None,
                range: whole,
                selection_range: selection,
                children,
            });
        }
        Some(top)
    }

    fn tokens(&mut self, uri: &Uri) -> Option<Vec<SemanticToken>> {
        let (analysis, text) = self.analysis(uri)?;
        let text = &*text;
        let mut marks: Vec<(Span, u32, u32)> = Vec::new();
        let symbol_of = |definition: Option<Span>| {
            definition.and_then(|span| analysis.symbols.iter().find(|s| s.span == span))
        };
        for symbol in &analysis.symbols {
            if let Some((token, modifiers)) = token_kind(symbol) {
                marks.push((name_span(text, symbol.span), token, modifiers | DECLARATION));
            }
        }
        for &span in &analysis.fixed_nouns {
            marks.push((span, FUNCTION, 0));
        }
        for reference in &analysis.references {
            let kind = match symbol_of(reference.definition) {
                Some(symbol) => token_kind(symbol),
                None if reference.definition.is_none() => self
                    .builtins
                    .iter()
                    .find(|b| b.name == reference.name)
                    .map(|builtin| match builtin.arguments {
                        Some(_) => (FUNCTION, DEFAULT_LIBRARY),
                        None => (VARIABLE, READONLY | DEFAULT_LIBRARY),
                    }),
                None => Some((VARIABLE, 0)),
            };
            if let Some((token, modifiers)) = kind {
                for word in words(text, reference.span) {
                    marks.push((name_span(text, word), token, modifiers));
                }
            }
        }
        let mut endings = Vec::new();
        for &(span, category) in &analysis.categories {
            let token = match category {
                Category::Flow => CONTROL,
                Category::Word => KEYWORD,
                Category::Field => PROPERTY,
                Category::Condition => {
                    endings.extend(condition_ending(text, span, &analysis));
                    continue;
                }
            };
            for word in words(text, span) {
                marks.push((name_span(text, word), token, 0));
            }
        }
        for (span, ..) in &mut marks {
            if let Some(ending) = endings
                .iter()
                .find(|e| e.start < span.end && span.start < e.end)
            {
                span.end = span.end.min(ending.start);
            }
        }
        marks.retain(|(span, ..)| span.start < span.end);
        marks.extend(endings.into_iter().map(|span| (span, CONTROL, 0)));
        marks.sort_by_key(|(span, ..)| span.start);
        let mut data = Vec::new();
        let mut last = Position::new(0, 0);
        let mut covered = 0;
        for (span, token, modifiers) in marks {
            if span.start < covered {
                continue;
            }
            covered = span.end;
            let start = position(text, span.start);
            let end = position(text, span.end);
            if start.line != end.line || end.character <= start.character {
                continue;
            }
            let delta_line = start.line - last.line;
            let delta_start = if delta_line == 0 {
                start.character - last.character
            } else {
                start.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: end.character - start.character,
                token_type: token,
                token_modifiers_bitset: modifiers,
            });
            last = start;
        }
        Some(data)
    }

    fn completion(
        &mut self,
        uri: &Uri,
        position: Position,
        spaced: bool,
    ) -> Option<CompletionResponse> {
        let (analysis, text) = self.analysis(uri)?;
        let text = &*text;
        let at = offset(text, position);
        let context = analysis::completion_context(text, at)?;
        let rest = text[at..].lines().next().unwrap_or_default();
        let previous = &context.previous;
        let known = analysis
            .symbols
            .iter()
            .any(|s| &s.name == previous && visible(text, s, at))
            || self.builtins.iter().any(|b| b.name == previous);
        let particle = analysis::trailing_particle(previous).filter(|_| !known && !context.quoted);
        if spaced && particle.is_none() {
            return None;
        }
        let nested = context.expression;
        let mut items = Vec::new();
        let mut order = 0;
        let mut verb = |lemma: &str, adjective: bool, particles: &[&str], doc: Option<String>| {
            if context.quoted {
                return;
            }
            order += 1;
            if particle.is_some_and(|particle| !particles.contains(&particle)) {
                return;
            }
            let used = analysis
                .references
                .iter()
                .filter(|r| r.name == lemma)
                .count();
            let rank = if used > 0 {
                10000 - used.min(9999)
            } else {
                10000 + order
            };
            for (place, (form, usage)) in analysis::verb_forms(lemma, adjective)
                .into_iter()
                .enumerate()
            {
                if (particle.is_some() || nested)
                    && nested != matches!(usage, FormUse::Value | FormUse::Modifier)
                {
                    continue;
                }
                let mut entry = item(
                    form.clone(),
                    CompletionItemKind::FUNCTION,
                    format!("{lemma}, {}", usage.label()),
                    doc.clone(),
                );
                if particle.is_some() {
                    if usage == FormUse::Statement && rest.trim().is_empty() {
                        entry.insert_text = Some(format!("{form}."));
                    }
                    entry.sort_text = Some(format!("{rank:06}{place}"));
                }
                items.push(entry);
            }
        };
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        let mut symbols: Vec<_> = analysis
            .symbols
            .iter()
            .filter(|symbol| visible(text, symbol, at))
            .collect();
        symbols.sort_by_key(|symbol| {
            (
                symbol
                    .scope
                    .map_or(usize::MAX, |scope| scope.end - scope.start),
                std::cmp::Reverse(symbol.span.start),
            )
        });
        for symbol in symbols {
            if !seen.insert(symbol.name.clone()) {
                continue;
            }
            if symbol.kind == SymbolKind::Function
                || (symbol.kind == SymbolKind::Imported && !symbol.lemmas.is_empty())
            {
                let lemma = symbol.lemmas.first().unwrap_or(&symbol.name);
                verb(lemma, symbol.adjective, &symbol.particles, None);
            } else {
                let kind = match symbol.kind {
                    SymbolKind::Record => CompletionItemKind::STRUCT,
                    SymbolKind::Constant => CompletionItemKind::CONSTANT,
                    _ => CompletionItemKind::VARIABLE,
                };
                let detail = line_of(text, symbol.span.start).trim().to_owned();
                names.push(item(symbol.name.clone(), kind, detail, None));
            }
        }
        for builtin in &self.builtins {
            if seen.contains(builtin.name) {
                continue;
            }
            match &builtin.arguments {
                Some(arguments) => {
                    let doc = format!("인자: {arguments}\n\n{}", builtin.description);
                    verb(builtin.name, false, &builtin.particles, Some(doc));
                }
                None => names.push(item(
                    builtin.name.to_owned(),
                    CompletionItemKind::CONSTANT,
                    "내장 상수".to_owned(),
                    Some(builtin.description.clone()),
                )),
            }
        }
        if particle.is_some() {
            for name in &mut names {
                name.sort_text = Some(format!("z{}", name.label));
            }
        }
        items.extend(names);
        if context.statement && particle.is_none() {
            for (label, body, detail) in SNIPPETS {
                items.push(CompletionItem {
                    label: (*label).to_owned(),
                    kind: Some(CompletionItemKind::SNIPPET),
                    detail: Some((*detail).to_owned()),
                    insert_text: Some((*body).to_owned()),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..CompletionItem::default()
                });
            }
        }
        let replacement = range(text, context.replacement);
        for entry in &mut items {
            let insertion = entry
                .insert_text
                .take()
                .unwrap_or_else(|| entry.label.clone());
            entry.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
                replacement,
                insertion,
            )));
        }
        Some(CompletionResponse::Array(items))
    }

    fn prepare_rename(&mut self, uri: &Uri, position: Position) -> Option<PrepareRenameResponse> {
        let (analysis, text, found) = self.lookup(uri, position)?;
        let text = &*text;
        let Target::Defined(definition) = found.target else {
            return None;
        };
        let symbol = analysis.symbol(definition)?;
        if !analysis::can_rename(symbol) {
            return None;
        }
        let name = name_span(text, found.word);
        let placeholder = if symbol.kind == SymbolKind::Function {
            symbol.name.clone()
        } else {
            text[name.start..name.end].to_owned()
        };
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: range(text, name),
            placeholder,
        })
    }

    fn rename(
        &mut self,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<WorkspaceEdit, String> {
        let (analysis, text) = self.analysis(uri).ok_or("문서가 열려 있지 않습니다.")?;
        let text = &*text;
        let found =
            find(&analysis, text, offset(text, position)).ok_or("이름을 찾지 못했습니다.")?;
        let Target::Defined(definition) = found.target else {
            return Err("내장 이름은 바꿀 수 없습니다.".to_owned());
        };
        let path = path_of(uri);
        let edits: Vec<_> = analysis::rename(
            path.as_deref(),
            text,
            &analysis,
            &self.sources,
            definition,
            new_name,
        )?
        .into_iter()
        .map(|edit| TextEdit::new(range(text, edit.span), edit.text.into_string()))
        .collect();
        if !self.versioned_edits {
            return Ok(WorkspaceEdit {
                changes: Some(HashMap::from([(uri.clone(), edits)])),
                ..WorkspaceEdit::default()
            });
        }
        Ok(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: self.documents.get(uri).map(|document| document.version),
                },
                edits: edits.into_iter().map(OneOf::Left).collect(),
            }])),
            ..WorkspaceEdit::default()
        })
    }

    fn code_actions(&self, uri: &Uri, diagnostics: &[Diagnostic]) -> Vec<CodeActionOrCommand> {
        diagnostics
            .iter()
            .filter_map(|diagnostic| {
                let fix = diagnostic.data.as_ref()?.get("fix")?;
                let text = fix.get("text")?.as_str()?.to_owned();
                let range: Range = serde_json::from_value(fix.get("range")?.clone()).ok()?;
                let title = format!("'{text}'{} 고치기", analysis::particle_after(&text, "로"));
                Some(CodeActionOrCommand::CodeAction(CodeAction {
                    title,
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(HashMap::from([(
                            uri.clone(),
                            vec![TextEdit::new(range, text)],
                        )])),
                        ..WorkspaceEdit::default()
                    }),
                    is_preferred: Some(true),
                    ..CodeAction::default()
                }))
            })
            .collect()
    }
}

const SNIPPETS: &[(&str, &str, &str)] = &[
    (
        "법",
        "`${1:값}`을 ${2:처리하는} 법:\n    `$1`을 반환한다.$0",
        "함수 정의",
    ),
    ("면", "${1:수가 0보다 크}면:\n    $0", "조건"),
    ("그렇지 않으면", "그렇지 않으면:\n    $0", "조건의 나머지"),
    (
        "마다",
        "${1:모음} 중 ${2:원소}마다:\n    $0",
        "모음의 원소마다 반복",
    ),
    (
        "부터 까지",
        "${1:1}부터 ${2:10}까지 ${3:수}마다:\n    $0",
        "범위 반복",
    ),
    ("동안", "${1:수가 0보다 큰} 동안:\n    $0", "조건 반복"),
    (
        "시도한다",
        "시도한다:\n    $1\n오류를 ${2:문제}로 받으면:\n    $0",
        "오류 처리",
    ),
    ("마지막으로", "마지막으로:\n    $0", "정리 블록"),
    ("함께", "함께:\n    $0", "기다리는 일을 함께 실행"),
    ("병렬로", "병렬로:\n    $0", "계산을 여러 코어에 나눠 실행"),
    (
        "가진다",
        "${1:자료형}은 ${2:필드}와 ${3:필드}를 가진다.",
        "자료형 정의",
    ),
    (
        "가져온다",
        "\"${1:파일}\"에서 ${2:이름}을 가져온다.",
        "다른 파일의 이름 가져오기",
    ),
    ("반환한다", "${1:값}을 반환한다.", "결과 돌려주기"),
    ("반복을 중단한다", "반복을 중단한다.", "반복 끝내기"),
    ("다음으로 진행한다", "다음으로 진행한다.", "다음 차례로"),
];

fn item(
    label: String,
    kind: CompletionItemKind,
    detail: String,
    doc: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail: Some(detail),
        documentation: doc.map(|value| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            })
        }),
        ..CompletionItem::default()
    }
}

enum Target {
    Defined(Span),
    Builtin(String),
}

struct Found {
    name: String,
    word: Span,
    target: Target,
}

fn find(analysis: &Analysis, text: &str, offset: usize) -> Option<Found> {
    let covers = |span: Span| span.start <= offset && offset <= span.end;
    for symbol in &analysis.symbols {
        let word = word_span(text, symbol.span);
        if covers(word) {
            return Some(Found {
                name: symbol.name.clone(),
                word,
                target: Target::Defined(symbol.span),
            });
        }
    }
    analysis.references.iter().find_map(|reference| {
        let word = word_span(text, reference.span);
        covers(word).then(|| Found {
            name: reference.name.clone(),
            word,
            target: match reference.definition {
                Some(span) => Target::Defined(span),
                None => Target::Builtin(reference.name.clone()),
            },
        })
    })
}

fn words(text: &str, span: Span) -> Vec<Span> {
    let slice = text.get(span.start..span.end).unwrap_or("");
    let mut found = Vec::new();
    let mut start = None;
    for (index, c) in slice.char_indices().chain([(slice.len(), ' ')]) {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(index),
            (true, Some(first)) => {
                found.push(Span::new(span.start + first, span.start + index));
                start = None;
            }
            _ => {}
        }
    }
    found
}

fn word_span(text: &str, span: Span) -> Span {
    let slice = text.get(span.start..span.end.min(text.len())).unwrap_or("");
    let end = slice.find(char::is_whitespace).unwrap_or(slice.len());
    Span::new(span.start, span.start + end)
}

fn name_span(text: &str, span: Span) -> Span {
    let slice = text.get(span.start..span.end).unwrap_or("");
    if slice.len() >= 2 && slice.starts_with('`') && slice.ends_with('`') {
        Span::new(span.start + 1, span.end - 1)
    } else {
        span
    }
}

fn visible(text: &str, symbol: &Symbol, offset: usize) -> bool {
    if symbol.kind == SymbolKind::Parameter && symbol.span.start > offset {
        return false;
    }
    let Some(scope) = symbol.scope else {
        return true;
    };
    if offset < scope.start {
        return false;
    }
    if offset <= scope.end {
        return true;
    }
    let indent = indentation(line_of(text, scope.end));
    text[scope.end..offset]
        .split('\n')
        .skip(1)
        .all(|line| line.trim().is_empty() || indentation(line) >= indent)
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn block_end(text: &str, line: usize) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let indent = lines.get(line).map_or(0, |l| indentation(l));
    let mut end_line = line;
    for (index, l) in lines.iter().enumerate().skip(line + 1) {
        if l.trim().is_empty() {
            continue;
        }
        if indentation(l) <= indent {
            break;
        }
        end_line = index;
    }
    lines
        .iter()
        .take(end_line + 1)
        .map(|l| l.len() + 1)
        .sum::<usize>()
        .saturating_sub(1)
        .min(text.len())
}

fn leaf(text: &str, symbol: &Symbol) -> DocumentSymbol {
    let selection = range(text, word_span(text, symbol.span));
    #[allow(deprecated)]
    DocumentSymbol {
        name: symbol.name.clone(),
        detail: None,
        kind: lsp_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: selection,
        selection_range: selection,
        children: None,
    }
}

fn lsp_kind(kind: SymbolKind) -> lsp_types::SymbolKind {
    match kind {
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Record => lsp_types::SymbolKind::STRUCT,
        SymbolKind::Constant => lsp_types::SymbolKind::CONSTANT,
        SymbolKind::Imported => lsp_types::SymbolKind::MODULE,
        SymbolKind::Variable | SymbolKind::Parameter => lsp_types::SymbolKind::VARIABLE,
    }
}

fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Variable => "변수",
        SymbolKind::Constant => "상수",
        SymbolKind::Parameter => "매개변수",
        SymbolKind::Function => "함수",
        SymbolKind::Record => "자료형",
        SymbolKind::Imported => "가져온 이름",
    }
}

fn condition_ending(text: &str, word: Span, analysis: &Analysis) -> Option<Span> {
    let written = text.get(word.start..word.end)?;
    let tail = |ending: &str| Span::new(word.end - ending.len(), word.end);
    if written == "면" || written == "이면" {
        return Some(word);
    }
    if written.ends_with("으면") {
        return Some(tail("으면"));
    }
    let copula = written.strip_suffix("이면").is_some_and(|host| {
        let end = word.start + host.len();
        matches!(
            host,
            "참" | "거짓" | "없음" | "이상" | "이하" | "초과" | "미만"
        ) || analysis
            .references
            .iter()
            .any(|r| r.span.end == end && r.span.start >= word.start)
    });
    if copula {
        Some(tail("이면"))
    } else {
        written.ends_with('면').then(|| tail("면"))
    }
}

fn token_kind(symbol: &analysis::Symbol) -> Option<(u32, u32)> {
    match symbol.kind {
        SymbolKind::Function => Some((FUNCTION, 0)),
        SymbolKind::Imported if !symbol.lemmas.is_empty() => Some((FUNCTION, 0)),
        SymbolKind::Variable | SymbolKind::Imported => Some((VARIABLE, 0)),
        SymbolKind::Parameter => Some((PARAMETER, 0)),
        SymbolKind::Constant => Some((VARIABLE, READONLY)),
        SymbolKind::Record => Some((TYPE, 0)),
    }
}

fn line_of(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    text[start..end].trim_end_matches('\r')
}

fn diagnostics(uri: &Uri, text: &str, sources: &SourceFiles) -> Vec<Diagnostic> {
    let saved = path_of(uri).map(|path| SourceFiles::canonical_path(&path));
    let Some(path) = saved else {
        return ieum::check(text.as_bytes())
            .iter()
            .map(|error| lsp_diagnostic(text, error, error.span))
            .collect();
    };
    ieum::check_sources(&path, sources)
        .iter()
        .filter_map(|error| {
            let location = error.location(error.error.diagnostic.span);
            (location.path == path)
                .then(|| lsp_diagnostic(text, &error.error.diagnostic, location.span))
        })
        .collect()
}

fn lsp_diagnostic(text: &str, error: &ieum::Diagnostic, span: Span) -> Diagnostic {
    let message = match &error.help {
        Some(help) => format!("{}\n도움말: {help}", error.message),
        None => error.message.to_string(),
    };
    let shift = span.start as isize - error.span.start as isize;
    let data = error.fix.as_ref().map(|fix| {
        let start = (fix.span.start as isize + shift).max(0) as usize;
        let end = (fix.span.end as isize + shift).max(0) as usize;
        serde_json::json!({
            "fix": {
                "range": range(text, Span::new(start, end)),
                "text": fix.text.as_ref(),
            }
        })
    });
    Diagnostic {
        range: range(text, span),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(error.code.to_owned())),
        source: Some("ieum".to_owned()),
        message,
        data,
        ..Diagnostic::default()
    }
}

fn range(text: &str, span: Span) -> Range {
    Range::new(position(text, span.start), position(text, span.end))
}

fn position(text: &str, offset: usize) -> Position {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].encode_utf16().count();
    Position::new(line as u32, column as u32)
}

fn offset(text: &str, position: Position) -> usize {
    let mut start = 0;
    for _ in 0..position.line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= position.character as usize || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    text.len()
}

fn formatted(text: &str) -> Option<Vec<TextEdit>> {
    let output = ieum::format_source(text.as_bytes()).ok()?;
    if output == text {
        return Some(Vec::new());
    }
    let end = position(text, text.len());
    Some(vec![TextEdit::new(
        Range::new(Position::new(0, 0), end),
        output,
    )])
}

fn path_of(uri: &Uri) -> Option<PathBuf> {
    if uri.scheme()?.as_str() != "file" {
        return None;
    }
    let path = uri.path().as_estr().decode().into_string().ok()?;
    if cfg!(windows) {
        let path = path.strip_prefix('/').unwrap_or(&path);
        Some(PathBuf::from(path.replace('/', "\\")))
    } else {
        Some(PathBuf::from(path.as_ref()))
    }
}

fn uri_of(path: &Path) -> Option<Uri> {
    let path = path.to_string_lossy().replace('\\', "/");
    let path = path.trim_start_matches("//?/");
    let encoded: String = path
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect();
    let prefix = if encoded.starts_with('/') {
        "file://"
    } else {
        "file:///"
    };
    format!("{prefix}{encoded}").parse().ok()
}
