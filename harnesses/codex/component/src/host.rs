//! The host side of Codex's model client and tool router.
//!
//! Upstream, Codex builds a Responses API request (`instructions`, `input`
//! items, `tools`, `reasoning`, `text.format`), streams server-sent events
//! back into `ResponseItem`s, and dispatches function calls through its tool
//! router. Here the request is a `cosmonic:agent/chat` call on a model the host
//! resolves from an alias, and function calls go to `cosmonic:agent/tools`.
//! Nothing in this module holds an endpoint, a key or a socket.
//!
//! The mapping is item by part. One host assistant message becomes one Codex
//! item per part, in order (text as an assistant `message`, reasoning as a
//! `reasoning` item, a tool use as a `function_call`, an unmodelled provider
//! block as `x_cosmonic_block`), and each part's continuation data rides on
//! its item. Replaying merges consecutive model items back into one assistant
//! message and consecutive outputs into one tool message, so what the host
//! returned is what it gets back, byte for byte where it asked for that.

use base64::Engine as _;

use crate::bindings::cosmonic::agent::inference_types as llm;
use crate::bindings::cosmonic::agent::tool_types as tt;
use crate::bindings::cosmonic::agent::{chat, models, tools};
use crate::bindings::{wit_future, wit_stream};
use crate::codex::models::{
    Citation, ContentItem, FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload,
    ReasoningItemReasoningSummary, ResponseItem,
};

/// The model alias this agent opens. The host maps it to a concrete model.
pub const MODEL_ALIAS: &str = "default";

// ----- history to the host's conversation -----

/// How content the host stores (`attachment-source.stored`) is written in
/// Codex's `image_url`: `x-cosmonic-stored:<media type>;<id>`, so it
/// round-trips. The attachment's `display-name` has no place in Codex's item
/// and is dropped.
const STORED_SCHEME: &str = "x-cosmonic-stored:";

/// The same, as conversations saved before the `stored` rename wrote it.
const LEGACY_BLOB_SCHEME: &str = "x-cosmonic-blob:";

/// A function call's arguments as the host takes them: a freeform tool's raw
/// text as `text`, anything else as `json`, unchanged either way.
pub fn tool_arguments(arguments: &str, freeform: bool) -> llm::ToolArguments {
    if freeform {
        llm::ToolArguments::Text(arguments.to_string())
    } else {
        llm::ToolArguments::Json(arguments.to_string())
    }
}

/// The host's arguments as a function call holds them: the text, and whether
/// it is a freeform tool's.
fn call_arguments(arguments: &llm::ToolArguments) -> (String, bool) {
    match arguments {
        llm::ToolArguments::Json(json) => (json.clone(), false),
        llm::ToolArguments::Text(text) => (text.clone(), true),
    }
}

fn attachment_from_url(url: &str) -> llm::Attachment {
    let attachment =
        |media_type: &str, source| llm::Attachment { media_type: media_type.to_string(), source, display_name: None };
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((media_type, data)) = rest.split_once(";base64,") {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                return attachment(media_type, llm::AttachmentSource::Bytes(bytes));
            }
        }
    }
    let stored = url.strip_prefix(STORED_SCHEME).or_else(|| url.strip_prefix(LEGACY_BLOB_SCHEME));
    if let Some((media_type, id)) = stored.and_then(|rest| rest.split_once(';')) {
        return attachment(media_type, llm::AttachmentSource::Stored(id.to_string()));
    }
    attachment("application/octet-stream", llm::AttachmentSource::Url(url.to_string()))
}

fn url_from_attachment(a: &llm::Attachment) -> String {
    match &a.source {
        llm::AttachmentSource::Bytes(bytes) => {
            format!("data:{};base64,{}", a.media_type, base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        llm::AttachmentSource::Url(url) => url.clone(),
        llm::AttachmentSource::Stored(id) => format!("{STORED_SCHEME}{};{id}", a.media_type),
    }
}

fn citations_to_host(citations: &[Citation]) -> Vec<llm::Citation> {
    citations
        .iter()
        .map(|c| llm::Citation { url: c.url.clone(), title: c.title.clone(), cited_text: c.cited_text.clone(), span: c.span })
        .collect()
}

fn citations_from_host(citations: &[llm::Citation]) -> Vec<Citation> {
    citations
        .iter()
        .map(|c| Citation { url: c.url.clone(), title: c.title.clone(), cited_text: c.cited_text.clone(), span: c.span })
        .collect()
}

fn tool_outputs(payload: &FunctionCallOutputPayload) -> Vec<llm::ToolOutput> {
    match &payload.body {
        FunctionCallOutputBody::Text(text) => vec![llm::ToolOutput::Text(text.clone())],
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => llm::ToolOutput::Text(text.clone()),
                FunctionCallOutputContentItem::InputImage { image_url } => {
                    llm::ToolOutput::Attachment(attachment_from_url(image_url))
                }
            })
            .collect(),
    }
}

/// The parts of one model item, in the host's terms.
fn assistant_parts(item: &ResponseItem) -> Vec<llm::AssistantPart> {
    match item {
        ResponseItem::Message { content, opaque, citations, .. } => content
            .iter()
            .map(|c| llm::AssistantPart {
                content: match c {
                    ContentItem::OutputText { text } | ContentItem::InputText { text } => {
                        llm::AssistantContent::Text(text.clone())
                    }
                    ContentItem::InputImage { image_url } => {
                        llm::AssistantContent::Attachment(attachment_from_url(image_url))
                    }
                    ContentItem::Refusal { refusal } => llm::AssistantContent::Refusal(refusal.clone()),
                },
                opaque: opaque.clone(),
                citations: citations_to_host(citations),
            })
            .collect(),
        ResponseItem::Reasoning { summary, encrypted_content, .. } => vec![llm::AssistantPart {
            content: llm::AssistantContent::Reasoning(llm::Reasoning {
                summary: summary
                    .iter()
                    .map(|s| match s {
                        ReasoningItemReasoningSummary::SummaryText { text } => text.clone(),
                    })
                    .collect(),
            }),
            opaque: encrypted_content.clone(),
            citations: Vec::new(),
        }],
        ResponseItem::FunctionCall { name, arguments, call_id, opaque, freeform, .. } => vec![llm::AssistantPart {
            content: llm::AssistantContent::ToolUse(llm::ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: tool_arguments(arguments, *freeform),
            }),
            opaque: opaque.clone(),
            citations: Vec::new(),
        }],
        ResponseItem::HostBlock { kind, json, opaque } => vec![llm::AssistantPart {
            content: llm::AssistantContent::Other(llm::OpaqueBlock { kind: kind.clone(), json: json.clone() }),
            opaque: opaque.clone(),
            citations: Vec::new(),
        }],
        _ => Vec::new(),
    }
}

/// Codex's `instructions` plus `input`, as one host conversation.
pub fn to_messages(system: &[String], items: &[ResponseItem]) -> Vec<llm::Message> {
    let mut out: Vec<llm::Message> = system.iter().cloned().map(llm::Message::System).collect();
    let mut assistant: Vec<llm::AssistantPart> = Vec::new();
    let mut results: Vec<llm::ToolResult> = Vec::new();
    fn flush(out: &mut Vec<llm::Message>, a: &mut Vec<llm::AssistantPart>, r: &mut Vec<llm::ToolResult>) {
        if !a.is_empty() {
            out.push(llm::Message::Assistant(llm::AssistantMessage { content: std::mem::take(a) }));
        }
        if !r.is_empty() {
            out.push(llm::Message::Tool(std::mem::take(r)));
        }
    }
    for item in items {
        match item {
            i if i.is_model_generated() => {
                if !results.is_empty() {
                    flush(&mut out, &mut assistant, &mut results);
                }
                assistant.extend(assistant_parts(i));
            }
            ResponseItem::FunctionCallOutput { call_id, output, .. } => {
                if !assistant.is_empty() {
                    out.push(llm::Message::Assistant(llm::AssistantMessage { content: std::mem::take(&mut assistant) }));
                }
                results.push(llm::ToolResult {
                    call_id: call_id.clone().unwrap_or_default(),
                    output: tool_outputs(output),
                    is_error: output.success == Some(false),
                });
            }
            ResponseItem::Message { role, content, .. } => {
                flush(&mut out, &mut assistant, &mut results);
                if role == "user" {
                    out.push(llm::Message::User(llm::UserMessage {
                        content: content
                            .iter()
                            .map(|c| match c {
                                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                                    llm::UserPart::Text(text.clone())
                                }
                                ContentItem::InputImage { image_url } => {
                                    llm::UserPart::Attachment(attachment_from_url(image_url))
                                }
                                ContentItem::Refusal { refusal } => llm::UserPart::Text(refusal.clone()),
                            })
                            .collect(),
                        name: None,
                    }));
                } else {
                    // developer and system messages
                    out.push(llm::Message::System(crate::codex::models::content_text(content)));
                }
            }
            _ => {}
        }
    }
    flush(&mut out, &mut assistant, &mut results);
    out
}

/// A completed host reply as Codex items, one per part, in order.
pub fn to_items(message: &llm::AssistantMessage) -> Vec<ResponseItem> {
    message
        .content
        .iter()
        .map(|part| {
            let opaque = part.opaque.clone();
            let message = |content: ContentItem, opaque| ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![content],
                opaque,
                citations: citations_from_host(&part.citations),
            };
            match &part.content {
                llm::AssistantContent::Text(text) => message(ContentItem::OutputText { text: text.clone() }, opaque),
                llm::AssistantContent::Refusal(text) => message(ContentItem::Refusal { refusal: text.clone() }, opaque),
                llm::AssistantContent::Reasoning(r) => ResponseItem::Reasoning {
                    id: None,
                    summary: r.summary.iter().map(|t| ReasoningItemReasoningSummary::SummaryText { text: t.clone() }).collect(),
                    content: None,
                    encrypted_content: opaque,
                },
                llm::AssistantContent::ToolUse(call) => {
                    let (arguments, freeform) = call_arguments(&call.arguments);
                    ResponseItem::FunctionCall {
                        id: None,
                        name: call.name.clone(),
                        arguments,
                        call_id: call.id.clone(),
                        opaque,
                        freeform,
                    }
                }
                llm::AssistantContent::Attachment(a) => {
                    message(ContentItem::InputImage { image_url: url_from_attachment(a) }, opaque)
                }
                llm::AssistantContent::Other(b) => {
                    ResponseItem::HostBlock { kind: b.kind.clone(), json: b.json.clone(), opaque }
                }
            }
        })
        .collect()
}

/// A tool's response as Codex's function call output.
pub fn output_of(response: tt::ToolResponse) -> FunctionCallOutputPayload {
    let mut items: Vec<FunctionCallOutputContentItem> = response
        .output
        .iter()
        .map(|o| match o {
            llm::ToolOutput::Text(text) => FunctionCallOutputContentItem::InputText { text: text.clone() },
            llm::ToolOutput::Attachment(a) if a.media_type.starts_with("image/") => {
                FunctionCallOutputContentItem::InputImage { image_url: url_from_attachment(a) }
            }
            llm::ToolOutput::Attachment(a) => FunctionCallOutputContentItem::InputText {
                text: format!("[a {} attachment the model cannot read]", a.media_type),
            },
        })
        .collect();
    if items.is_empty() {
        if let Some(structured) = response.structured {
            items.push(FunctionCallOutputContentItem::InputText { text: structured });
        }
    }
    let body = match items.as_slice() {
        [FunctionCallOutputContentItem::InputText { text }] => FunctionCallOutputBody::Text(text.clone()),
        _ => FunctionCallOutputBody::ContentItems(items),
    };
    FunctionCallOutputPayload { body, success: Some(!response.is_error) }
}

/// A failed tool call as the output the model reads. "unsupported call" is
/// Codex's own text for a function it does not know.
pub fn output_of_error(name: &str, error: &tt::CallError) -> FunctionCallOutputPayload {
    use tt::{CallError as C, DispatchError as D, OutcomeError as O};
    let text = match error {
        C::NotDispatched(D::UnknownTool(_)) => format!("unsupported call: {name}"),
        C::NotDispatched(D::InvalidArguments(m)) => format!("invalid arguments: {m}"),
        C::NotDispatched(D::Denied(m)) => format!("the call was denied: {m}"),
        C::NotDispatched(D::Cancelled) => "the call was cancelled before it ran".into(),
        C::NotDispatched(D::TimedOut) => "the call timed out before it ran".into(),
        C::NotDispatched(D::Upstream(m) | D::Other(m)) => format!("the call was not made: {m}"),
        C::OutcomeUnknown(O::Cancelled) => "the call was cancelled; it may or may not have taken effect".into(),
        C::OutcomeUnknown(O::TimedOut) => "the call timed out; it may or may not have taken effect".into(),
        C::OutcomeUnknown(O::Upstream(m) | O::Other(m)) => {
            format!("the call failed: {m}; it may or may not have taken effect")
        }
    };
    FunctionCallOutputPayload { body: FunctionCallOutputBody::Text(text), success: Some(false) }
}

// ----- the calls -----

pub async fn list_tools() -> Result<Vec<tt::ToolDef>, String> {
    tools::list().await.map_err(|e| match e {
        tt::ListError::Unavailable(m) => format!("tools are unavailable: {m}"),
        tt::ListError::Denied(m) => format!("listing tools was denied: {m}"),
        tt::ListError::Other(m) => m,
    })
}

pub async fn call_tool(call: llm::ToolCall) -> Result<tt::ToolResponse, tt::CallError> {
    tools::call(call).await
}

/// What the host knows about the model behind the alias, if it says.
pub async fn context_length() -> Option<u32> {
    models::describe(MODEL_ALIAS.to_string()).await.ok().and_then(|d| d.context_length)
}

/// Why a model call did not produce a reply.
#[derive(Debug, Clone)]
pub enum SampleError {
    /// `models.open` refused the alias.
    Open(String),
    /// `chat` refused the request before accepting it.
    Refused(llm::RequestError),
    /// The reply failed after it started.
    Failed(llm::GenerationError),
    /// The reader of the run went away while the reply streamed.
    Cancelled,
}

impl std::fmt::Display for SampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SampleError::Open(m) => write!(f, "the host has no model for this agent: {m}"),
            SampleError::Refused(e) => write!(f, "the host refused the model call: {e:?}"),
            SampleError::Failed(e) => write!(f, "the model call failed: {e:?}"),
            SampleError::Cancelled => write!(f, "the run was cancelled"),
        }
    }
}

/// Where a reply's live text goes.
pub trait Deltas {
    /// Returns false once the reader has gone away.
    async fn text(&mut self, text: &str) -> bool;
    async fn reasoning(&mut self, text: &str) -> bool;
}

pub struct Sampled {
    pub completion: llm::Completion,
}

/// One model call: Codex's `ModelClientSession::stream`, over `chat`.
pub async fn sample(
    messages: Vec<llm::Message>,
    options: llm::ChatOptions,
    deltas: &mut impl Deltas,
) -> Result<Sampled, SampleError> {
    let model = models::open(MODEL_ALIAS.to_string()).await.map_err(|e| SampleError::Open(format!("{e:?}")))?;
    // Streams do not buffer: messages are written while `chat` is pending,
    // then `messages-done` says the conversation is complete.
    let (mut tx, rx) = wit_stream::new::<llm::Message>();
    let (done_tx, done_rx) = wit_future::new::<Result<(), String>>(|| Err("the agent stopped writing".into()));
    let write = async move {
        let unwritten = tx.write_all(messages).await;
        drop(tx);
        let verdict = if unwritten.is_empty() { Ok(()) } else { Err(format!("{} messages undelivered", unwritten.len())) };
        let _ = done_tx.write(verdict).await;
    };
    let ((), started) = futures::future::join(write, chat::chat(&model, rx, done_rx, options)).await;
    let (mut chunks, outcome) = started.map_err(SampleError::Refused)?;
    while let Some(chunk) = chunks.next().await {
        let alive = match &chunk.delta {
            llm::ChunkDelta::Text(t) if !t.is_empty() => deltas.text(t).await,
            llm::ChunkDelta::ReasoningText(t) if !t.is_empty() => deltas.reasoning(t).await,
            // Tool calls and whole parts are taken from the completion,
            // which is authoritative.
            _ => true,
        };
        if !alive {
            // Dropping the chunk stream asks the host to stop generating.
            drop(chunks);
            return Err(SampleError::Cancelled);
        }
    }
    let completion = outcome.await.map_err(SampleError::Failed)?;
    if !completion.adaptations.is_empty() {
        let what = completion.adaptations.iter().map(|a| a.detail.as_str()).collect::<Vec<_>>().join("; ");
        eprintln!("codex-agent: the host adapted this request: {what}");
    }
    drop(model);
    Ok(Sampled { completion })
}

pub fn chat_options(
    tools: &[tt::ToolDef],
    schema: Option<&str>,
    effort: Option<llm::Effort>,
    trace_parent: Option<String>,
) -> llm::ChatOptions {
    llm::ChatOptions {
        max_tokens: None,
        temperature: None,
        top_k: None,
        top_p: None,
        min_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        penalty_last_n: None,
        logit_bias: Vec::new(),
        seed: None,
        stop: Vec::new(),
        // Codex's `text.format` json_schema, when the driver set one.
        response_format: match schema {
            Some(s) => llm::ResponseFormat::JsonSchema(s.to_string()),
            None => llm::ResponseFormat::Text,
        },
        tools: tools
            .iter()
            .map(|t| llm::ToolDef {
                name: t.name.clone(),
                title: t.title.clone(),
                description: t.description.clone(),
                // As the host listed it: a freeform tool stays freeform, and
                // its input reaches it as the model wrote it.
                input: t.input.clone(),
                output_schema: t.output_schema.clone(),
            })
            .collect(),
        tool_choice: None,
        // Codex sends reasoning effort and summary settings from its model
        // catalog. This agent has none: effort is the run's when the driver
        // set one, and otherwise both take the host's default.
        thinking: None,
        effort,
        // Calls in one reply run one after another (`run_tools`), so the
        // model may ask for several; the host's default stands.
        parallel_tool_calls: None,
        meta: llm::RequestMeta {
            trace_parent,
            deadline_ms: None,
            cache_key: None,
            cache_points: Vec::new(),
            labels: Vec::new(),
        },
        // Adapt and report rather than refuse: the run continues with what
        // the host could represent.
        on_unrepresentable: llm::UnrepresentablePolicy::Adapt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(content: llm::AssistantContent, opaque: Option<&str>) -> llm::AssistantPart {
        llm::AssistantPart { content, opaque: opaque.map(str::to_string), citations: Vec::new() }
    }

    #[test]
    fn a_reply_round_trips_with_its_continuation_data() {
        let reply = llm::AssistantMessage {
            content: vec![
                part(llm::AssistantContent::Reasoning(llm::Reasoning { summary: vec!["plan".into()] }), Some("enc-1")),
                part(llm::AssistantContent::Text("Checking.".into()), None),
                part(
                    llm::AssistantContent::ToolUse(llm::ToolCall {
                        id: "c1".into(),
                        name: "echo".into(),
                        arguments: llm::ToolArguments::Json("{}".into()),
                    }),
                    Some("sig"),
                ),
                part(llm::AssistantContent::Other(llm::OpaqueBlock { kind: "server_tool".into(), json: "{}".into() }), None),
            ],
        };
        let mut items = vec![ResponseItem::user_text("hi")];
        items.extend(to_items(&reply));
        assert!(matches!(&items[1], ResponseItem::Reasoning { encrypted_content: Some(e), .. } if e == "enc-1"));
        items.push(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("c1".into()),
            output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text("ok".into()), success: Some(true) },
        });
        let messages = to_messages(&["sys".into()], &items);
        assert_eq!(messages.len(), 4, "system, user, one assistant message, one tool message");
        let llm::Message::Assistant(back) = &messages[2] else { panic!("not assistant") };
        assert_eq!(back.content.len(), 4);
        assert_eq!(back.content[0].opaque.as_deref(), Some("enc-1"));
        assert_eq!(back.content[2].opaque.as_deref(), Some("sig"));
        assert!(matches!(&back.content[3].content, llm::AssistantContent::Other(b) if b.kind == "server_tool"));
        let llm::Message::Tool(results) = &messages[3] else { panic!("not tool") };
        assert_eq!(results[0].call_id, "c1");
        assert!(!results[0].is_error);
    }

    #[test]
    fn a_freeform_call_round_trips_as_text() {
        let patch = "*** Begin Patch\n{not json}\n*** End Patch\n";
        let call = llm::ToolCall { id: "c1".into(), name: "patch".into(), arguments: llm::ToolArguments::Text(patch.into()) };
        let reply = llm::AssistantMessage { content: vec![part(llm::AssistantContent::ToolUse(call), None)] };
        let mut items = to_items(&reply);
        assert!(matches!(&items[0], ResponseItem::FunctionCall { arguments, freeform: true, .. } if arguments == patch));
        items.push(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("c1".into()),
            output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text("ok".into()), success: Some(true) },
        });
        let messages = to_messages(&[], &items);
        let back = messages
            .iter()
            .find_map(|m| match m {
                llm::Message::Assistant(a) => Some(a),
                _ => None,
            })
            .expect("no assistant message");
        assert!(matches!(&back.content[0].content, llm::AssistantContent::ToolUse(c)
            if matches!(&c.arguments, llm::ToolArguments::Text(t) if t == patch)));
    }

    #[test]
    fn stored_attachments_round_trip_and_old_blob_urls_still_read() {
        let a = attachment_from_url("x-cosmonic-blob:image/png;id-1");
        assert!(matches!(&a.source, llm::AttachmentSource::Stored(id) if id == "id-1"));
        assert_eq!(url_from_attachment(&a), "x-cosmonic-stored:image/png;id-1");
        let again = attachment_from_url(&url_from_attachment(&a));
        assert!(again.media_type == "image/png" && matches!(&again.source, llm::AttachmentSource::Stored(id) if id == "id-1"));
    }

    #[test]
    fn image_tool_output_becomes_an_attachment_again() {
        let response = tt::ToolResponse {
            output: vec![
                llm::ToolOutput::Text("a plot".into()),
                llm::ToolOutput::Attachment(llm::Attachment {
                    media_type: "image/png".into(),
                    source: llm::AttachmentSource::Bytes(vec![1, 2, 3]),
                    display_name: None,
                }),
            ],
            structured: None,
            is_error: false,
        };
        let payload = output_of(response);
        let outputs = tool_outputs(&payload);
        assert!(matches!(&outputs[1], llm::ToolOutput::Attachment(a)
            if a.media_type == "image/png" && matches!(&a.source, llm::AttachmentSource::Bytes(b) if b == &[1, 2, 3])));
    }
}
