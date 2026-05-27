//! Tool-use stability harness — Sprint 1 gate before Sprint 2.
//!
//! Sprint 2's multi-agent runtime hinges on the LLM emitting one of a small
//! set of structured tool calls (BROADCAST / ASK_AGENT / ANSWER / DONE) every
//! turn. If the model frequently emits free text or malformed JSON, the
//! router can't dispatch and the entire mechanism collapses.
//!
//! This harness runs the same scenario N times against each candidate model,
//! and records whether the response:
//!   1. used exactly one tool call (not free text, not multiple)
//!   2. called one of the allowed tool names
//!   3. produced JSON-parseable arguments
//!   4. produced arguments matching the schema (required fields present)
//!
//! Run with:
//!     cargo run --example tool_use_stability --release
//!
//! Requires `~/.aidock/api_keys.json` to have a "bailian" entry — same source
//! the app uses, so set it up by running the app once and finishing BYOK.

use std::time::Instant;

use aidock_lib::keyring_store;
use aidock_lib::llm::{
    bailian::BailianProvider, ChatMessage, ChatRequest, LLMProvider, Tool,
};
use serde_json::{json, Value as JsonValue};

const MODELS: &[&str] = &["qwen3.6-plus", "qwen3-max-2026-01-23"];
const ITERATIONS_PER_MODEL: usize = 10;

fn agent_tools() -> Vec<Tool> {
    vec![
        Tool::function(
            "BROADCAST",
            "Send a message to the entire team. Use to kick off a discussion or announce status to everyone.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The message body" },
                    "topic_id": { "type": "string", "description": "Topic this message belongs to" }
                },
                "required": ["content", "topic_id"]
            }),
        ),
        Tool::function(
            "ASK_AGENT",
            "Send a targeted question to one specific teammate. They MUST respond. Use when you need a specific person's input.",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "enum": ["frontend_dev", "backend_dev"], "description": "The teammate's role id" },
                    "content": { "type": "string", "description": "Your question" },
                    "topic_id": { "type": "string" }
                },
                "required": ["to", "content", "topic_id"]
            }),
        ),
        Tool::function(
            "ANSWER",
            "Answer a previous ASK_AGENT directed at you.",
            json!({
                "type": "object",
                "properties": {
                    "reply_to": { "type": "string", "description": "The message id you are answering" },
                    "content": { "type": "string" }
                },
                "required": ["reply_to", "content"]
            }),
        ),
        Tool::function(
            "DONE",
            "Mark your current task complete. Use ONLY when you've actually finished delivering something.",
            json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "What you delivered" }
                },
                "required": ["summary"]
            }),
        ),
    ]
}

const ALLOWED_NAMES: &[&str] = &["BROADCAST", "ASK_AGENT", "ANSWER", "DONE"];

struct Scenario {
    name: &'static str,
    system_prompt: &'static str,
    user_prompt: &'static str,
}

fn scenarios() -> Vec<Scenario> {
    vec![Scenario {
        name: "kickoff",
        system_prompt: "You are the Product Manager (role id: PM) in a 3-person AI software team. \
            Your teammates are frontend_dev and backend_dev. \
            When you respond, you MUST call exactly one of the provided tools — never reply in plain text. \
            Pick the tool that best fits the situation.",
        user_prompt: "I want to build a TODO web app. Users can sign up, log in, and manage a personal task list. \
            Please get the team going.",
    }]
}

#[derive(Default, Debug, Clone, Copy)]
struct ModelStats {
    iterations: usize,
    used_one_tool: usize,
    allowed_name: usize,
    valid_json_args: usize,
    valid_schema: usize,
}

impl ModelStats {
    fn record(&mut self, outcome: &Outcome) {
        self.iterations += 1;
        if outcome.used_one_tool { self.used_one_tool += 1; }
        if outcome.allowed_name { self.allowed_name += 1; }
        if outcome.valid_json_args { self.valid_json_args += 1; }
        if outcome.valid_schema { self.valid_schema += 1; }
    }

    fn pct(&self, count: usize) -> f32 {
        if self.iterations == 0 { 0.0 } else { (count as f32 / self.iterations as f32) * 100.0 }
    }
}

#[derive(Debug)]
struct Outcome {
    used_one_tool: bool,
    allowed_name: bool,
    valid_json_args: bool,
    valid_schema: bool,
    tool_name: Option<String>,
    error: Option<String>,
    elapsed_ms: u128,
}

fn validate_schema(name: &str, args: &JsonValue) -> bool {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return false,
    };
    let required: &[&str] = match name {
        "BROADCAST" => &["content", "topic_id"],
        "ASK_AGENT" => &["to", "content", "topic_id"],
        "ANSWER" => &["reply_to", "content"],
        "DONE" => &["summary"],
        _ => return false,
    };
    required.iter().all(|k| obj.get(*k).is_some_and(|v| !v.is_null()))
}

async fn run_one(provider: &BailianProvider, model: &str, scenario: &Scenario) -> Outcome {
    let started = Instant::now();
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::System { content: scenario.system_prompt.into() },
            ChatMessage::User { content: scenario.user_prompt.into() },
        ],
        temperature: Some(0.3),
        max_tokens: Some(2048),
        tools: agent_tools(),
        tool_choice: Some(json!("auto")),
    };

    let resp = match provider.chat_completion(req).await {
        Ok(r) => r,
        Err(e) => {
            return Outcome {
                used_one_tool: false,
                allowed_name: false,
                valid_json_args: false,
                valid_schema: false,
                tool_name: None,
                error: Some(e.to_string()),
                elapsed_ms: started.elapsed().as_millis(),
            };
        }
    };

    let elapsed_ms = started.elapsed().as_millis();
    let calls = &resp.tool_calls;
    let used_one_tool = calls.len() == 1;
    let first = calls.first();

    let (allowed_name, valid_json_args, valid_schema, tool_name) = match first {
        None => (false, false, false, None),
        Some(c) => {
            let name = c.function.name.clone();
            let allowed = ALLOWED_NAMES.contains(&name.as_str());
            let parsed: Result<JsonValue, _> = serde_json::from_str(&c.function.arguments);
            let valid_json = parsed.is_ok();
            let valid_schema = match &parsed {
                Ok(v) => validate_schema(&name, v),
                Err(_) => false,
            };
            (allowed, valid_json, valid_schema, Some(name))
        }
    };

    Outcome {
        used_one_tool,
        allowed_name,
        valid_json_args,
        valid_schema,
        tool_name,
        error: None,
        elapsed_ms,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let key = keyring_store::load_api_key("bailian")?
        .ok_or_else(|| anyhow::anyhow!(
            "No 'bailian' API key found in ~/.aidock/api_keys.json. \
             Launch the AiDock app once and complete the BYOK setup first."
        ))?;

    let scenarios = scenarios();
    let provider = BailianProvider::new(key);

    println!("Tool-use stability harness");
    println!("  scenarios:  {}", scenarios.len());
    println!("  iterations: {} per model", ITERATIONS_PER_MODEL);
    println!("  models:     {:?}", MODELS);
    println!();

    for model in MODELS {
        println!("─── {} ───", model);
        let mut stats = ModelStats::default();
        for (scenario_idx, scenario) in scenarios.iter().enumerate() {
            for iter in 1..=ITERATIONS_PER_MODEL {
                let outcome = run_one(&provider, model, scenario).await;
                let marker = if outcome.valid_schema { "✓" } else { "✗" };
                let tail = match (&outcome.error, &outcome.tool_name) {
                    (Some(err), _) => format!("ERROR: {err}"),
                    (None, Some(n)) => format!(
                        "tool={n} one={} allowed={} json={} schema={}",
                        outcome.used_one_tool,
                        outcome.allowed_name,
                        outcome.valid_json_args,
                        outcome.valid_schema,
                    ),
                    (None, None) => "no tool call (free-text response)".to_string(),
                };
                println!(
                    "  [{:>2}.{:>2}] {:<12} {} {:>5}ms  {}",
                    scenario_idx + 1,
                    iter,
                    scenario.name,
                    marker,
                    outcome.elapsed_ms,
                    tail
                );
                stats.record(&outcome);
            }
        }
        println!(
            "  ── summary: used_one_tool {:.0}% | allowed_name {:.0}% | valid_json {:.0}% | valid_schema {:.0}%",
            stats.pct(stats.used_one_tool),
            stats.pct(stats.allowed_name),
            stats.pct(stats.valid_json_args),
            stats.pct(stats.valid_schema),
        );
        let gate = stats.pct(stats.valid_schema);
        let verdict = if gate >= 90.0 {
            "GREEN — proceed to Sprint 2"
        } else if gate >= 70.0 {
            "YELLOW — needs prompt tuning before Sprint 2"
        } else {
            "RED — Sprint 2 mechanism at significant risk; investigate model or schema"
        };
        println!("  verdict ({}): {}", model, verdict);
        println!();
    }

    Ok(())
}
