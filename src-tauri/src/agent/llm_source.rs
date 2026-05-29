//! ④.a — the real [`ActionSource`]: an LLM-backed decision maker.
//!
//! This is the seam between ④.a's pure loop ([`crate::agent::react`]) and the
//! provider transport (⑤). It owns the agent's running conversation (the ④.b
//! context lives behind here, not in the loop) and translates the model's
//! `tool_calls` into the loop's [`Action`] vocabulary.
//!
//! Wiring decisions made here (flagged so they're easy to revisit):
//!   1. **One action per loop iteration.** A single LLM response may carry
//!      several tool calls; we enqueue them and hand them to the loop one at a
//!      time across iterations. This keeps the OpenAI wire contract valid
//!      (every `tool_call` gets exactly one `tool` response) while preserving
//!      the loop's "one act → one observe" shape.
//!   2. **No tool calls → end the turn.** If the model answers with plain
//!      text and no tool call, we treat that text as its final word (`Done`),
//!      rather than looping until the iteration backstop trips.
//!   3. **Two-pipe rule (④ side).** [`observe`] folds a *token-aware summary*
//!      of the tool result back into the LLM context — never the full body.
//!      The full body is the executor's concern (it goes to the UI/⑥ event).
//!   4. **ASK_AGENT → Wait.** Mapping a question onto the suspend outcome.
//!      Actually delivering the ASK to the team is ③'s job, wired later.

use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::json;

use crate::agent::protocol::{
    TOOL_ANSWER, TOOL_ASK_AGENT, TOOL_BROADCAST, TOOL_DONE, TOOL_PROGRESS, TOOL_SUMMARY,
    TOOL_WORK_START,
};
use crate::agent::react::{
    Action, ActionSource, Observation, Outbound, Plan, PlanStep, StepStatus, TurnState,
};
use crate::llm::{ChatMessage, ChatRequest, LLMProvider, Tool};

/// Tool the model calls to declare or rewrite its checklist — the `SetPlan`
/// action's wire form. Internal to ④.a (not a cross-agent message), so it
/// lives here next to the source rather than in `protocol.rs`.
pub const TOOL_SET_PLAN: &str = "set_plan";

/// How many chars of a tool result we feed back to the model. The full body
/// already reached the UI/⑥ via the executor's event; the LLM only needs
/// enough to decide its next move (two-pipe rule). Placeholder until ④.b does
/// something smarter than a flat cap.
const LLM_RESULT_SUMMARY_MAX: usize = 800;

// ---------------------------------------------------------------------------
// set_plan tool schema + parsing
// ---------------------------------------------------------------------------

/// The `set_plan` tool definition, to be advertised alongside the message /
/// aux / MCP tools when an agent runs under the ReAct loop.
pub fn set_plan_tool() -> Tool {
    Tool::function(
        TOOL_SET_PLAN,
        "Declare or update your plan: the overall goal plus an ordered checklist of steps. \
         Call this first to lay out your approach, and again whenever the plan changes — \
         mark steps done/failed as you go (you own the statuses, the runtime does not advance them).",
        json!({
            "type": "object",
            "properties": {
                "goal": { "type": "string", "description": "One-line statement of what this turn achieves." },
                "steps": {
                    "type": "array",
                    "description": "Ordered milestones. Re-send the whole list each time, updating statuses.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "done", "failed"],
                                "description": "Defaults to pending."
                            }
                        },
                        "required": ["description"]
                    }
                }
            },
            "required": ["goal", "steps"]
        }),
    )
}

#[derive(Debug, Deserialize)]
struct SetPlanArgs {
    goal: String,
    #[serde(default)]
    steps: Vec<SetPlanStep>,
}

#[derive(Debug, Deserialize)]
struct SetPlanStep {
    description: String,
    #[serde(default = "default_step_status")]
    status: StepStatus,
}

fn default_step_status() -> StepStatus {
    StepStatus::Pending
}

/// Extract a `content`-ish field from an outward message tool's args so a
/// `Speak` action carries something readable. Falls back to the raw args.
#[derive(Debug, Deserialize)]
struct ContentArg {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    task: Option<String>,
}

/// Pure translation: one LLM tool call → one [`Action`]. Kept free-standing so
/// it can be unit-tested without a provider.
pub fn translate(tool: &str, raw_args: &str) -> Action {
    match tool {
        TOOL_SET_PLAN => match serde_json::from_str::<SetPlanArgs>(raw_args) {
            Ok(args) => {
                let steps = args
                    .steps
                    .into_iter()
                    .enumerate()
                    .map(|(i, s)| PlanStep {
                        seq: i,
                        description: s.description,
                        status: s.status,
                    })
                    .collect();
                Action::SetPlan(Plan {
                    goal: args.goal,
                    steps,
                })
            }
            // Malformed plan: surface as a no-op-ish plan so the loop can ask
            // the model to retry rather than crashing.
            Err(e) => Action::SetPlan(Plan {
                goal: format!("(unparsable set_plan: {e})"),
                steps: Vec::new(),
            }),
        },

        TOOL_DONE => {
            let summary = serde_json::from_str::<ContentArg>(raw_args)
                .ok()
                .and_then(|a| a.summary.or(a.content))
                .unwrap_or_else(|| "done".to_string());
            Action::Done { summary }
        }

        TOOL_ASK_AGENT => {
            // Carries the full question; ③ dispatches it and parks. The
            // recipient id is also what WAITING_ANSWER keys on.
            #[derive(Deserialize)]
            struct AskArgs {
                #[serde(default)]
                to: Option<String>,
                #[serde(default)]
                content: Option<String>,
                #[serde(default)]
                expected_format: Option<String>,
            }
            let a = serde_json::from_str::<AskArgs>(raw_args).unwrap_or(AskArgs {
                to: None,
                content: None,
                expected_format: None,
            });
            Action::Wait {
                to: a.to.unwrap_or_else(|| "someone".to_string()),
                content: a.content.unwrap_or_default(),
                expected_format: a.expected_format,
            }
        }

        // ANSWER keeps its reply_to so ③ can thread it back to the right ASK.
        TOOL_ANSWER => {
            #[derive(Deserialize)]
            struct AnswerArgs {
                #[serde(default)]
                reply_to: Option<String>,
                #[serde(default)]
                content: Option<String>,
            }
            let a = serde_json::from_str::<AnswerArgs>(raw_args).unwrap_or(AnswerArgs {
                reply_to: None,
                content: None,
            });
            Action::Speak(Outbound::Answer {
                reply_to: a.reply_to.unwrap_or_default(),
                content: a.content.unwrap_or_default(),
            })
        }

        // Remaining outward kinds collapse into a Broadcast for MVP — the
        // loop doesn't care which it is. (Proper WORK_START / PROGRESS /
        // SUMMARY handling deferred; see ④.b / topic work.)
        TOOL_BROADCAST | TOOL_PROGRESS | TOOL_WORK_START | TOOL_SUMMARY => {
            let content = serde_json::from_str::<ContentArg>(raw_args)
                .ok()
                .and_then(|a| a.content.or(a.summary).or(a.task))
                .unwrap_or_else(|| raw_args.to_string());
            Action::Speak(Outbound::Broadcast { content })
        }

        // Everything else (fs__*, query, scratchpad) is a concrete operation.
        other => Action::ToolCall {
            tool: other.to_string(),
            args: raw_args.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// LlmSource
// ---------------------------------------------------------------------------

/// LLM-backed [`ActionSource`]. Generic over the provider so tests inject a
/// scripted fake and never hit the network.
pub struct LlmSource<P> {
    provider: P,
    model: String,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<Tool>,
    /// The running conversation — this IS the ④.b context for now.
    messages: Vec<ChatMessage>,
    /// Actions parsed from the latest response, not yet handed to the loop.
    queue: VecDeque<(Option<String>, Action)>,
    /// tool_call_id of the `ToolCall` we last returned and are awaiting an
    /// observation for, so `observe` can attach the result to it.
    awaiting_obs: Option<String>,
    /// The resume answer we've already folded into context, so we don't
    /// re-inject it on every iteration of a resumed turn.
    folded_answer: Option<String>,
}

impl<P: LLMProvider> LlmSource<P> {
    /// `initial_messages` is the seed context (system prompt + recovered
    /// history). `tools` must already include [`set_plan_tool`] plus whatever
    /// message/aux/MCP tools the role is allowed.
    pub fn new(
        provider: P,
        model: impl Into<String>,
        temperature: f32,
        max_tokens: u32,
        tools: Vec<Tool>,
        initial_messages: Vec<ChatMessage>,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            temperature,
            max_tokens,
            tools,
            messages: initial_messages,
            queue: VecDeque::new(),
            awaiting_obs: None,
            folded_answer: None,
        }
    }

    fn push_tool_response(&mut self, tool_call_id: &str, content: impl Into<String>) {
        self.messages.push(ChatMessage::Tool {
            content: content.into(),
            tool_call_id: tool_call_id.to_string(),
        });
    }
}

impl<P: LLMProvider> ActionSource for LlmSource<P> {
    async fn next_action(&mut self, state: &TurnState) -> Action {
        // Fold a resume answer into context exactly once.
        if let Some(ans) = &state.pending_answer {
            if self.folded_answer.as_ref() != Some(ans) {
                self.messages.push(ChatMessage::User {
                    content: format!("[answer received] {ans}"),
                });
                self.folded_answer = Some(ans.clone());
            }
        }

        loop {
            // Drain queued actions first (one per iteration).
            if let Some((id, action)) = self.queue.pop_front() {
                match &action {
                    // ToolCall's result comes via `observe` — remember its id.
                    Action::ToolCall { .. } => self.awaiting_obs = id,
                    // The rest produce no execution result, so close their
                    // tool_call now to keep the wire contract satisfied.
                    Action::SetPlan(_) => {
                        if let Some(id) = id {
                            self.push_tool_response(&id, "plan recorded");
                        }
                    }
                    Action::Speak(_) => {
                        if let Some(id) = id {
                            self.push_tool_response(&id, "dispatched to the team");
                        }
                    }
                    Action::Wait { .. } => {
                        if let Some(id) = id {
                            self.push_tool_response(&id, "question sent; awaiting answer");
                        }
                    }
                    Action::Done { .. } => {
                        if let Some(id) = id {
                            self.push_tool_response(&id, "turn complete");
                        }
                    }
                }
                return action;
            }

            // Queue empty — ask the model what to do next.
            let req = ChatRequest {
                model: self.model.clone(),
                messages: self.messages.clone(),
                temperature: Some(self.temperature),
                max_tokens: Some(self.max_tokens),
                tools: self.tools.clone(),
                tool_choice: Some(json!("auto")),
            };

            let resp = match self.provider.chat_completion(req).await {
                Ok(r) => r,
                Err(e) => {
                    // Transport failure ends the turn cleanly rather than
                    // spinning; the runtime surfaces the Done summary.
                    return Action::Done {
                        summary: format!("LLM transport error: {e}"),
                    };
                }
            };

            self.messages.push(ChatMessage::Assistant {
                content: resp.content.clone(),
                tool_calls: resp.tool_calls.clone(),
            });

            if resp.tool_calls.is_empty() {
                // Decision #2: plain text with no tool call = final word.
                return Action::Done {
                    summary: resp.content.unwrap_or_else(|| "(no action)".to_string()),
                };
            }

            for tc in resp.tool_calls {
                let action = translate(&tc.function.name, &tc.function.arguments);
                self.queue.push_back((Some(tc.id), action));
            }
            // Loop back to pop the first queued action.
        }
    }

    fn observe(&mut self, obs: &Observation) {
        if let Some(id) = self.awaiting_obs.take() {
            // Two-pipe rule (④ side): summary, not full body.
            let summary = summarize_for_llm(&obs.raw);
            self.push_tool_response(&id, summary);
        }
    }
}

/// Token-aware shrink of a tool result for the LLM context. Char-boundary safe.
fn summarize_for_llm(raw: &str) -> String {
    if raw.len() <= LLM_RESULT_SUMMARY_MAX {
        return raw.to_string();
    }
    let mut end = LLM_RESULT_SUMMARY_MAX;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} bytes; full body in tool-call log]", &raw[..end], raw.len() - end)
}

// ---------------------------------------------------------------------------
// Tests — translation purity + end-to-end with a scripted fake provider.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::react::{
        ActionExecutor, AlwaysContinue, ExecResult, NullSink, PlanLoop, TurnOutcome,
    };
    use crate::llm::{ChatResponse, FunctionCall, LLMError, ToolCall};
    use std::sync::Mutex;

    // --- translation unit tests -------------------------------------------

    #[test]
    fn translate_set_plan_with_statuses() {
        let action = translate(
            TOOL_SET_PLAN,
            r#"{"goal":"ship login","steps":[
                {"description":"write API","status":"done"},
                {"description":"write tests"}
            ]}"#,
        );
        match action {
            Action::SetPlan(plan) => {
                assert_eq!(plan.goal, "ship login");
                assert_eq!(plan.steps.len(), 2);
                assert_eq!(plan.steps[0].status, StepStatus::Done);
                assert_eq!(plan.steps[1].status, StepStatus::Pending); // defaulted
                assert_eq!(plan.steps[1].seq, 1);
            }
            other => panic!("expected SetPlan, got {other:?}"),
        }
    }

    #[test]
    fn translate_fs_tool_is_toolcall() {
        let action = translate("fs__write_file", r#"{"path":"a.md","content":"x"}"#);
        assert!(matches!(action, Action::ToolCall { tool, .. } if tool == "fs__write_file"));
    }

    #[test]
    fn translate_done_and_ask() {
        assert!(matches!(
            translate(TOOL_DONE, r#"{"summary":"shipped","topic_id":"t-1"}"#),
            Action::Done { summary } if summary == "shipped"
        ));
        assert!(matches!(
            translate(TOOL_ASK_AGENT, r#"{"to":"backend_dev","content":"?","topic_id":"t-1"}"#),
            Action::Wait { to, .. } if to == "backend_dev"
        ));
    }

    // --- fake provider ----------------------------------------------------

    struct FakeProvider {
        responses: Mutex<VecDeque<ChatResponse>>,
    }

    impl FakeProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    impl LLMProvider for FakeProvider {
        fn provider_id(&self) -> &'static str {
            "fake"
        }
        fn supports_tool_use(&self) -> bool {
            true
        }
        async fn chat_completion(&self, _req: ChatRequest) -> Result<ChatResponse, LLMError> {
            Ok(self.responses.lock().unwrap().pop_front().unwrap_or(ChatResponse {
                content: Some("(fake exhausted)".into()),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            }))
        }
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn resp(tool_calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls,
            finish_reason: "tool_calls".into(),
            usage: None,
        }
    }

    struct OkExecutor;
    impl ActionExecutor for OkExecutor {
        async fn execute(&mut self, _tool: &str, _args: &str) -> ExecResult {
            ExecResult {
                raw: "wrote 1 file".into(),
                success: true,
            }
        }
    }

    fn source(responses: Vec<ChatResponse>) -> LlmSource<FakeProvider> {
        LlmSource::new(
            FakeProvider::new(responses),
            "qwen-test",
            0.7,
            1024,
            vec![set_plan_tool()],
            vec![ChatMessage::System {
                content: "you are a test agent".into(),
            }],
        )
    }

    // --- end-to-end: LlmSource drives a real PlanLoop ---------------------

    #[tokio::test]
    async fn end_to_end_plan_act_done() {
        // Response 1: set_plan. Response 2: one fs write. Response 3: DONE.
        let responses = vec![
            resp(vec![call(
                "c1",
                TOOL_SET_PLAN,
                r#"{"goal":"make README","steps":[{"description":"write it"}]}"#,
            )]),
            resp(vec![call(
                "c2",
                "fs__write_file",
                r#"{"path":"README.md","content":"hi"}"#,
            )]),
            resp(vec![call("c3", TOOL_DONE, r#"{"summary":"README written"}"#)]),
        ];
        let mut lp = PlanLoop::new(source(responses), OkExecutor, AlwaysContinue, NullSink);

        let (outcome, state) = lp.run_turn().await;

        assert_eq!(
            outcome,
            TurnOutcome::Done {
                summary: "README written".into()
            }
        );
        assert!(state.plan.is_some());
        assert_eq!(state.replan_count, 0);
    }

    #[tokio::test]
    async fn multiple_tool_calls_in_one_response_drain_across_iterations() {
        // One response carries set_plan AND the fs write; next is DONE.
        let responses = vec![
            resp(vec![
                call(
                    "c1",
                    TOOL_SET_PLAN,
                    r#"{"goal":"g","steps":[{"description":"s"}]}"#,
                ),
                call("c2", "fs__write_file", r#"{"path":"a","content":"b"}"#),
            ]),
            resp(vec![call("c3", TOOL_DONE, r#"{"summary":"ok"}"#)]),
        ];
        let mut lp = PlanLoop::new(source(responses), OkExecutor, AlwaysContinue, NullSink);

        let (outcome, _state) = lp.run_turn().await;

        assert_eq!(outcome, TurnOutcome::Done { summary: "ok".into() });
    }

    #[tokio::test]
    async fn plain_text_no_tool_call_ends_turn() {
        let responses = vec![ChatResponse {
            content: Some("I think we should discuss requirements first.".into()),
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: None,
        }];
        let mut lp = PlanLoop::new(source(responses), OkExecutor, AlwaysContinue, NullSink);

        let (outcome, _state) = lp.run_turn().await;

        assert!(matches!(outcome, TurnOutcome::Done { summary } if summary.contains("requirements")));
    }
}
