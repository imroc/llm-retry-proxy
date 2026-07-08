/// Protocol transform: OpenAI Responses API → Chat Completions API.
///
/// Enables tools like OpenAI Codex (which use /v1/responses) to work
/// with any Chat Completions compatible API provider.
///
/// Handles:
/// - Request: /v1/responses input → /v1/chat/completions messages
/// - Non-streaming response: Chat Completions JSON → Responses API JSON
/// - Streaming response: Chat Completions SSE → Responses API SSE
/// - Function calling, reasoning content, multi-turn tool conversations
use bytes::Bytes;
use serde_json::{json, Value};

pub const RESPONSES_TO_CHAT: &str = "responses_to_chat";

// ── Request conversion ─────────────────────────────────────────────────────

/// Convert a Responses API request body to Chat Completions request body.
pub fn responses_to_chat_request(body: &[u8]) -> Option<Bytes> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let chat = build_chat_request(&body);
    serde_json::to_vec(&chat).ok().map(Bytes::from)
}

fn build_chat_request(body: &Value) -> Value {
    let mut req = json!({
        "model": body.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "messages": input_to_messages(body),
        "stream": body.get("stream").unwrap_or(&json!(false)),
    });

    if let Some(v) = body.get("temperature") {
        if !v.is_null() {
            req["temperature"] = v.clone();
        }
    }
    if let Some(v) = body.get("max_output_tokens") {
        req["max_tokens"] = v.clone();
    }
    if let Some(v) = body.get("top_p") {
        req["top_p"] = v.clone();
    }

    // reasoning.effort → reasoning_effort
    if let Some(reasoning) = body.get("reasoning") {
        if let Some(effort) = reasoning.get("effort") {
            req["reasoning_effort"] = effort.clone();
        }
    }

    // Convert tools: Responses API flat format → Chat Completions nested
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let mut chat_tools: Vec<Value> = Vec::new();
        for t in tools {
            if t.get("type").and_then(|v| v.as_str()) != Some("function") {
                continue;
            }
            let fn_obj = if t.get("function").is_some() {
                t["function"].clone()
            } else {
                t.clone()
            };
            chat_tools.push(json!({
                "type": "function",
                "function": {
                    "name": fn_obj.get("name").unwrap_or(&json!("")),
                    "description": fn_obj.get("description").unwrap_or(&json!("")),
                    "parameters": fn_obj.get("parameters").unwrap_or(&json!({})),
                }
            }));
        }
        if !chat_tools.is_empty() {
            req["tools"] = json!(chat_tools);
        }
    }

    req
}

fn input_to_messages(body: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // instructions → system message
    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        messages.push(json!({"role": "system", "content": instructions}));
    }

    let input = body.get("input");

    if input.is_none() || matches!(input, Some(Value::Null)) {
        return json!(messages);
    }

    // String input → single user message
    if let Some(s) = input.and_then(|v| v.as_str()) {
        messages.push(json!({"role": "user", "content": s}));
        return json!(messages);
    }

    let items = match input.and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return json!(messages),
    };

    let mut pending_tool_calls: Vec<Value> = Vec::new();

    let flush = |pending: &mut Vec<Value>, msgs: &mut Vec<Value>| {
        if !pending.is_empty() {
            msgs.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": pending.clone(),
            }));
            pending.clear();
        }
    };

    for item in items {
        // String item → user message
        if let Some(s) = item.as_str() {
            flush(&mut pending_tool_calls, &mut messages);
            messages.push(json!({"role": "user", "content": s}));
            continue;
        }

        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };

        let item_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // function_call → assistant tool_calls
        if item_type == "function_call" {
            let call_id = obj
                .get("call_id")
                .or(obj.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            pending_tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": {
                    "name": obj.get("name").unwrap_or(&json!("")),
                    "arguments": obj.get("arguments").unwrap_or(&json!("{}")),
                },
            }));
            continue;
        }

        // function_call_output → tool message
        if item_type == "function_call_output" {
            flush(&mut pending_tool_calls, &mut messages);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": obj.get("call_id").unwrap_or(&json!("")),
                "content": obj.get("output").unwrap_or(&json!("")),
            }));
            continue;
        }

        // Regular message
        flush(&mut pending_tool_calls, &mut messages);
        let mut role = obj
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user")
            .to_string();
        if role == "developer" {
            role = "system".to_string();
        }

        let content = match obj.get("content") {
            Some(Value::Array(parts)) => {
                let texts: Vec<&str> = parts
                    .iter()
                    .filter_map(|c| {
                        let ct = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match ct {
                            "input_text" => c.get("text").and_then(|v| v.as_str()),
                            "input_image" => Some("[image]"),
                            _ => c.get("text").and_then(|v| v.as_str()),
                        }
                    })
                    .collect();
                texts.join("\n")
            }
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };

        messages.push(json!({"role": role, "content": content}));
    }

    flush(&mut pending_tool_calls, &mut messages);
    json!(messages)
}

// ── Non-streaming response conversion ──────────────────────────────────────

/// Convert a Chat Completions JSON response to Responses API format.
pub fn chat_response_to_responses(body: &[u8]) -> Option<Bytes> {
    let chat: Value = serde_json::from_slice(body).ok()?;

    let model = chat.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let empty_arr = vec![];
    let choices = chat
        .get("choices")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_arr);
    let empty_msg = json!({});
    let message = choices
        .first()
        .and_then(|c| c.get("message"))
        .unwrap_or(&empty_msg);
    let content_text = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut content_parts: Vec<Value> = vec![json!({
        "type": "output_text",
        "text": content_text,
        "annotations": [],
    })];

    // tool_calls → function_call content parts
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let empty_fn = json!({});
            let fn_obj = tc.get("function").unwrap_or(&empty_fn);
            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            content_parts.push(json!({
                "type": "function_call",
                "id": tc_id,
                "call_id": tc_id,
                "name": fn_obj.get("name").unwrap_or(&json!("")),
                "arguments": fn_obj.get("arguments").unwrap_or(&json!("{}")),
            }));
        }
    }

    let output_item = json!({
        "id": make_id("msg"),
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": content_parts,
    });

    let empty_usage = json!({});
    let usage = chat.get("usage").unwrap_or(&empty_usage);

    let result = json!({
        "id": make_id("resp"),
        "object": "response",
        "created_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "status": "completed",
        "model": model,
        "output": [output_item],
        "usage": {
            "input_tokens": usage.get("prompt_tokens").unwrap_or(&json!(0)),
            "output_tokens": usage.get("completion_tokens").unwrap_or(&json!(0)),
            "total_tokens": usage.get("total_tokens").unwrap_or(&json!(0)),
        },
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": {"effort": "medium", "summary": "auto"},
        "text": {"format": {"type": "text"}},
        "tools": [],
        "truncation": "disabled",
    });

    serde_json::to_vec(&result).ok().map(Bytes::from)
}

/// Convert a Chat Completions SSE stream to Responses API SSE format.
///
/// Returns a stream of SSE data lines (already formatted as `data: {...}\n\n`).
/// The original upstream SSE chunks are consumed and converted in real time.
pub fn transform_chat_sse_chunk(line: &str, state: &mut StreamTransformState) -> Option<String> {
    // Must start with "data: "
    let data_str = line.strip_prefix("data: ")?.trim();

    if data_str == "[DONE]" {
        return flush_stream_completion(state);
    }

    let chunk: Value = serde_json::from_str(data_str).ok()?;
    transform_sse_chunk(&chunk, state)
}

/// State for streaming SSE transformation.
pub struct StreamTransformState {
    pub msg_id: String,
    pub resp_id: String,
    pub model: String,
    pub created: u64,
    pub full_text: String,
    pub total_input: u64,
    pub total_output: u64,
    pub msg_closed: bool,
    pub output_index: usize, // 0 when msg is open, 1 after msg closed
    pub active_tool_calls: Vec<ToolCallState>,
    pub completed_tool_calls: Vec<ToolCallState>,
    /// Emitted header events? (response.created, in_progress, output_item.added, content_part.added)
    pub headers_emitted: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallState {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub arguments: String,
    pub output_index: usize,
}

impl StreamTransformState {
    pub fn new(model: &str, reasoning_effort: Option<&str>) -> Self {
        let _ = reasoning_effort;
        Self {
            msg_id: make_id("msg"),
            resp_id: make_id("resp"),
            model: model.to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            full_text: String::new(),
            total_input: 0,
            total_output: 0,
            msg_closed: false,
            output_index: 0,
            active_tool_calls: Vec::new(),
            completed_tool_calls: Vec::new(),
            headers_emitted: false,
        }
    }
}

fn transform_sse_chunk(chunk: &Value, state: &mut StreamTransformState) -> Option<String> {
    let mut output = String::new();

    // Emit header events on first chunk
    if !state.headers_emitted {
        state.headers_emitted = true;
        output.push_str(&emit_stream_headers(state));
    }

    // Handle usage-only chunks
    let choices = chunk.get("choices").and_then(|v| v.as_array());

    if choices.is_none() || choices.map(|a| a.is_empty()) == Some(true) {
        if let Some(u) = chunk.get("usage") {
            state.total_input = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            state.total_output = u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }
        return if output.is_empty() {
            None
        } else {
            Some(output)
        };
    }

    let choices = choices.unwrap();
    if choices.is_empty() {
        return None;
    }

    let empty_obj = json!({});
    let delta = choices[0].get("delta").unwrap_or(&empty_obj);
    let finish_reason = choices[0].get("finish_reason");

    // reasoning content
    if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            output.push_str(&sse(json!({
                "type": "response.reasoning_text.delta",
                "item_id": state.msg_id,
                "output_index": 0,
                "content_index": 0,
                "delta": r,
            })));
        }
    }

    // text content
    if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            state.full_text.push_str(text);
            output.push_str(&sse(json!({
                "type": "response.output_text.delta",
                "item_id": state.msg_id,
                "output_index": 0,
                "content_index": 0,
                "delta": text,
            })));
        }
    }

    // tool calls
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let tc_index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let empty_fn = json!({});
            let fn_obj = tc.get("function").unwrap_or(&empty_fn);

            let existing = state.active_tool_calls.iter().find(|t| t.index == tc_index);

            if existing.is_none() {
                // New tool call — close text message first
                if !state.msg_closed {
                    output.push_str(&close_msg_item(state));
                }

                let new_tc = ToolCallState {
                    id: tc_id.to_string(),
                    index: tc_index,
                    name: fn_obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    arguments: fn_obj
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    output_index: state.output_index + tc_index,
                };

                output.push_str(&sse(json!({
                    "type": "response.output_item.added",
                    "output_index": new_tc.output_index,
                    "item": {
                        "id": new_tc.id,
                        "type": "function_call",
                        "call_id": new_tc.id,
                        "name": new_tc.name,
                        "arguments": "",
                        "status": "in_progress",
                    },
                })));

                state.active_tool_calls.push(new_tc);
            } else if let Some(existing) = state
                .active_tool_calls
                .iter_mut()
                .find(|t| t.index == tc_index)
            {
                let args_delta = fn_obj
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                existing.arguments.push_str(args_delta);
                output.push_str(&sse(json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": existing.id,
                    "output_index": existing.output_index,
                    "delta": args_delta,
                })));
            }
        }
    }

    // finish_reason == "tool_calls": close msg and complete tool calls
    if finish_reason.and_then(|v| v.as_str()) == Some("tool_calls") {
        if !state.msg_closed {
            output.push_str(&close_msg_item(state));
        }
        for tc in &state.active_tool_calls {
            output.push_str(&sse(json!({
                "type": "response.function_call_arguments.done",
                "item_id": tc.id,
                "output_index": tc.output_index,
                "arguments": tc.arguments,
            })));
            output.push_str(&sse(json!({
                "type": "response.output_item.done",
                "output_index": tc.output_index,
                "item": {
                    "id": tc.id,
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments,
                    "status": "completed",
                },
            })));
        }
        state.completed_tool_calls = state.active_tool_calls.clone();
        state.active_tool_calls.clear();
    }

    if let Some(u) = chunk.get("usage") {
        state.total_input = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        state.total_output = u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn emit_stream_headers(state: &StreamTransformState) -> String {
    let empty = json!({
        "id": state.resp_id,
        "object": "response",
        "created_at": state.created,
        "status": "in_progress",
        "model": &state.model,
        "output": [],
        "usage": null,
    });

    let mut out = String::new();
    out.push_str(&sse(json!({"type": "response.created", "response": empty})));
    out.push_str(&sse(
        json!({"type": "response.in_progress", "response": empty}),
    ));
    out.push_str(&sse(json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {"id": state.msg_id, "type": "message", "role": "assistant", "status": "in_progress", "content": []},
    })));
    out.push_str(&sse(json!({
        "type": "response.content_part.added",
        "output_index": 0,
        "content_index": 0,
        "part": {"type": "output_text", "text": "", "annotations": []},
    })));
    out
}

fn close_msg_item(state: &mut StreamTransformState) -> String {
    if state.msg_closed {
        return String::new();
    }
    state.msg_closed = true;
    let mut out = String::new();
    out.push_str(&sse(json!({
        "type": "response.content_part.done",
        "output_index": 0,
        "content_index": 0,
        "part": {"type": "output_text", "text": state.full_text, "annotations": []},
    })));
    out.push_str(&sse(json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "id": state.msg_id,
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": state.full_text, "annotations": []}],
        },
    })));
    state.output_index = 1;
    out
}

fn flush_stream_completion(state: &mut StreamTransformState) -> Option<String> {
    let mut out = String::new();

    // Close text message if still open
    if !state.msg_closed {
        out.push_str(&close_msg_item(state));
    }

    // Complete remaining tool calls (some models don't send finish_reason=tool_calls)
    for tc in &state.active_tool_calls {
        if !state.completed_tool_calls.iter().any(|c| c.id == tc.id) {
            out.push_str(&sse(json!({
                "type": "response.function_call_arguments.done",
                "item_id": tc.id,
                "output_index": tc.output_index,
                "arguments": tc.arguments,
            })));
            out.push_str(&sse(json!({
                "type": "response.output_item.done",
                "output_index": tc.output_index,
                "item": {
                    "id": tc.id,
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments,
                    "status": "completed",
                },
            })));
        }
    }
    // Include all completed tool calls
    let all_completed: Vec<&ToolCallState> = state
        .completed_tool_calls
        .iter()
        .chain(
            state
                .active_tool_calls
                .iter()
                .filter(|a| !state.completed_tool_calls.iter().any(|c| c.id == a.id)),
        )
        .collect();

    let mut output_items: Vec<Value> = Vec::new();
    if !state.full_text.is_empty() {
        output_items.push(json!({
            "id": state.msg_id,
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": state.full_text, "annotations": []}],
        }));
    }
    for tc in &all_completed {
        output_items.push(json!({
            "id": tc.id,
            "type": "function_call",
            "call_id": tc.id,
            "name": tc.name,
            "arguments": tc.arguments,
            "status": "completed",
        }));
    }

    out.push_str(&sse(json!({
        "type": "response.completed",
        "response": {
            "id": state.resp_id,
            "object": "response",
            "created_at": state.created,
            "status": "completed",
            "model": &state.model,
            "output": output_items,
            "usage": {
                "input_tokens": state.total_input,
                "output_tokens": state.total_output,
                "total_tokens": state.total_input + state.total_output,
            },
            "parallel_tool_calls": true,
            "previous_response_id": null,
            "reasoning": {"effort": "medium", "summary": "auto"},
            "text": {"format": {"type": "text"}},
            "tools": [],
            "truncation": "disabled",
        },
    })));
    out.push_str("data: [DONE]\n\n");
    Some(out)
}

fn make_id(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string().replace('-', "");
    format!("{}_{}", prefix, &id[..24])
}

fn sse(data: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_to_messages_simple_string() {
        let body = json!({
            "model": "gpt-4",
            "instructions": "You are helpful.",
            "input": "Hello"
        });
        let msgs = input_to_messages(&body);
        let arr = msgs.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"], "system");
        assert_eq!(arr[0]["content"], "You are helpful.");
        assert_eq!(arr[1]["role"], "user");
        assert_eq!(arr[1]["content"], "Hello");
    }

    #[test]
    fn test_input_to_messages_with_tools() {
        let body = json!({
            "model": "gpt-4",
            "input": [
                {"role": "user", "content": "What is the weather?"},
                {"type": "function_call", "id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "Sunny, 20C"},
            ]
        });
        let msgs = input_to_messages(&body);
        let arr = msgs.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["role"], "user");
        // function_call → assistant with tool_calls
        assert_eq!(arr[1]["role"], "assistant");
        let tc = arr[1]["tool_calls"][0].clone();
        assert_eq!(tc["function"]["name"], "get_weather");
        // function_call_output → tool message
        assert_eq!(arr[2]["role"], "tool");
        assert_eq!(arr[2]["content"], "Sunny, 20C");
    }

    #[test]
    fn test_responses_to_chat_request() {
        let body = json!({
            "model": "test-model",
            "instructions": "Be helpful",
            "input": "hi",
            "stream": true,
            "reasoning": {"effort": "high"}
        });
        let result = responses_to_chat_request(&serde_json::to_vec(&body).unwrap()).unwrap();
        let chat: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(chat["model"], "test-model");
        assert_eq!(chat["stream"], true);
        assert_eq!(chat["reasoning_effort"], "high");
        assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_chat_response_to_responses_non_streaming() {
        let chat = json!({
            "model": "test",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let result = chat_response_to_responses(&serde_json::to_vec(&chat).unwrap()).unwrap();
        let resp: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(resp["object"], "response");
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["model"], "test");
    }
}
