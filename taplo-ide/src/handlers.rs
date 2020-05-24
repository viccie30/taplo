use crate::{Document, World};
use lsp_async_stub::{rpc::Error, Context, Params, RequestWriter};
use lsp_types::*;
use taplo::{formatter, util::coords::Mapper};
use wasm_bindgen_futures::spawn_local;

mod diagnostics;
mod document_symbols;
mod folding_ranges;
mod semantic_tokens;

pub async fn initialize(
    context: Context<World>,
    _params: Params<InitializeParams>,
) -> Result<InitializeResult, Error> {
    // Update configuration after initialization.
    // !! This might cause race conditions with this response,
    // !! it is fine in the single-threaded wasm environment.
    spawn_local(update_configuration(context));

    Ok(InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::Full)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: false.into(),
                    },
                    legend: SemanticTokensLegend {
                        token_types: semantic_tokens::TokenType::LEGEND.into(),
                        token_modifiers: Vec::new(),
                    },
                    range_provider: None,
                    document_provider: Some(SemanticTokensDocumentProvider::Bool(true)),
                }),
            ),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            document_symbol_provider: Some(true),
            document_formatting_provider: Some(true),
            // execute_command_provider: Some(ExecuteCommandOptions {
            //     commands: vec!["evenBetterToml.toJson".into()],
            //     work_done_progress_options: Default::default(),
            // }),
            ..Default::default()
        },
        server_info: Some(ServerInfo {
            name: "ebToml".into(),
            version: Some("1.0.0".into()),
        }),
    })
}

async fn update_configuration(mut context: Context<World>) {
    let res = context
        .write_request::<request::WorkspaceConfiguration, _>(Some(ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: None,
                section: Some("evenBetterToml".into()),
            }],
        }))
        .await
        .unwrap()
        .into_result();

    let mut config_vals = match res {
        Ok(v) => v,
        Err(e) => panic!(e),
    };

    let mut w = context.world().lock().await;

    w.configuration = serde_json::from_value(config_vals.remove(0)).unwrap_or_default();
}

pub async fn configuration_change(
    context: Context<World>,
    _params: Params<DidChangeConfigurationParams>,
) {
    update_configuration(context).await;
}

pub async fn document_open(mut context: Context<World>, params: Params<DidOpenTextDocumentParams>) {
    let p = match params.optional() {
        None => return,
        Some(p) => p,
    };

    let parse = taplo::parser::parse(&p.text_document.text);

    let mapper = Mapper::new(&p.text_document.text, true);

    let diag = PublishDiagnosticsParams {
        uri: p.text_document.uri.clone(),
        diagnostics: diagnostics::collect_diagnostics(&p.text_document.uri, &parse, &mapper),
        version: None,
    };

    context.world().lock().await.documents.insert(
        p.text_document.uri,
        Document {
            parse,
            mapper,
        },
    );

    context
        .write_notification::<notification::PublishDiagnostics, _>(Some(diag))
        .await
        .ok();
}

pub async fn document_change(
    mut context: Context<World>,
    params: Params<DidChangeTextDocumentParams>,
) {
    let mut p = match params.optional() {
        None => return,
        Some(p) => p,
    };

    // We expect one full change
    let change = match p.content_changes.pop() {
        None => return,
        Some(c) => c,
    };

    let parse = taplo::parser::parse(&change.text);
    let mapper = Mapper::new(&change.text, true);

    let diag = PublishDiagnosticsParams {
        uri: p.text_document.uri.clone(),
        diagnostics: diagnostics::collect_diagnostics(&p.text_document.uri, &parse, &mapper),
        version: None,
    };

    context.world().lock().await.documents.insert(
        p.text_document.uri,
        Document {
            parse,
            mapper,
        },
    );

    context
        .write_notification::<notification::PublishDiagnostics, _>(Some(diag))
        .await
        .ok();
}

pub async fn semantic_tokens(
    mut context: Context<World>,
    params: Params<SemanticTokensParams>,
) -> Result<Option<SemanticTokensResult>, Error> {
    let p = params.required()?;

    let w = context.world().lock().await;
    let doc = w
        .documents
        .get(&p.text_document.uri)
        .ok_or_else(Error::invalid_params)?;

    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: semantic_tokens::create_tokens(&doc.parse.clone().into_syntax(), &doc.mapper),
    })))
}

pub async fn folding_ranges(
    mut context: Context<World>,
    params: Params<FoldingRangeParams>,
) -> Result<Option<Vec<FoldingRange>>, Error> {
    let p = params.required()?;

    let w = context.world().lock().await;

    let doc = w
        .documents
        .get(&p.text_document.uri)
        .ok_or_else(Error::invalid_params)?;

    Ok(Some(folding_ranges::create_folding_ranges(
        &doc.parse.clone().into_syntax(),
        &doc.mapper,
    )))
}

pub async fn document_symbols(
    mut context: Context<World>,
    params: Params<DocumentSymbolParams>,
) -> Result<Option<DocumentSymbolResponse>, Error> {
    let p = params.required()?;

    let w = context.world().lock().await;

    let doc = w
        .documents
        .get(&p.text_document.uri)
        .ok_or_else(Error::invalid_params)?;

    Ok(Some(DocumentSymbolResponse::Nested(
        document_symbols::create_symbols(&doc),
    )))
}

pub async fn format(
    mut context: Context<World>,
    params: Params<DocumentFormattingParams>,
) -> Result<Option<Vec<TextEdit>>, Error> {
    let p = params.required()?;

    let w = context.world().lock().await;

    let doc = w
        .documents
        .get(&p.text_document.uri)
        .ok_or_else(Error::invalid_params)?;

    let mut format_opts = formatter::Options::default();

    if let Some(v) = w.configuration.formatter.array_auto_collapse {
        format_opts.array_auto_collapse = v;
    }

    if let Some(v) = w.configuration.formatter.array_auto_expand {
        format_opts.array_auto_expand = v;
    }

    if let Some(v) = w.configuration.formatter.column_width {
        format_opts.column_width = v;
    }

    if let Some(v) = w.configuration.formatter.array_trailing_comma {
        format_opts.array_trailing_comma = v;
    }

    if let Some(v) = w.configuration.formatter.trailing_newline {
        format_opts.trailing_newline = v;
    }

    if let Some(v) = w.configuration.formatter.indent_string.clone() {
        format_opts.indent_string = v;
    } else {
        format_opts.indent_string = if p.options.insert_spaces {
            " ".repeat(p.options.tab_size as usize)
        } else {
            "\t".into()
        }
    }

    if let Some(v) = w.configuration.formatter.indent_tables {
        format_opts.indent_tables = v;
    }

    if let Some(v) = w.configuration.formatter.crlf {
        format_opts.crlf = v;
    }

    if let Some(v) = w.configuration.formatter.reorder_keys {
        format_opts.reorder_keys = v;
    }

    Ok(Some(vec![TextEdit {
        range: doc.mapper.all_range(),
        new_text: taplo::formatter::format_syntax(doc.parse.clone().into_syntax(), format_opts),
    }]))
}