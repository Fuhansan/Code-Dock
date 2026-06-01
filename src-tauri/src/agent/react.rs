//! ④.a — ReAct 计划循环（骨架）。
//!
//! See CLAUDE.md block ④.a. This module owns a single agent's *internal*
//! per-turn state machine: the `plan → act → observe → (replan) → ...` loop
//! that replaces the degenerate "one message → one LLM call" reflex still
//! living in `runtime.rs`.
//!
//! Deliberate non-goals of THIS file (kept out so the foundation stays clean):
//!   - real LLM transport            → lives behind the [`ActionSource`] seam (⑤)
//!   - real fs side effects          → lives behind the [`ActionExecutor`] seam (④.d's hook)
//!   - cross-agent routing / WAITING → owned by ③; the loop only *emits* a
//!     `Wait` outcome and lets the runtime do the parking/waking
//!   - context assembly              → owned by ④.b; the [`ActionSource`] impl
//!     pulls its own context, the driver never builds prompts
//!
//! Who owns "which step am I on / is it done": **the model**, not the loop.
//! The plan is the model's advisory artifact (Claude Code reconciliation in
//! CLAUDE.md ④.a) — it maintains step statuses by re-emitting `SetPlan`. The
//! loop never auto-advances a step. Its only governors are the two hard
//! backstops below: consecutive tool-call failures and the total iteration
//! budget. These replace the human "press Esc" that Claude Code leans on and
//! AiDock lacks (multi-agent, no one watching every turn).
//!
//! Why the seams are traits and not concrete calls: per the test strategy in
//! CLAUDE.md ④.a, the LLM is the one thing we must be able to fake so the loop
//! logic can be tested deterministically without ever hitting a real model.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Tunables (CLAUDE.md ④.a「失败重试」hard backstops — values TBD, hand-waved
// until we can calibrate against real runs).
// ---------------------------------------------------------------------------

/// K — this many tool-call failures *in a row* → forced escalate. Resets on
/// any success or replan, so it measures "stuck", not "ever failed".
pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: usize = 3;

/// N — total loop iterations before forced escalate. Pure runaway guard for a
/// model that never commits to `Done` (e.g. SetPlan ↔ SetPlan flapping). Kept
/// generous because real coding turns legitimately span many tool calls
/// (scaffolding a project = 14+ file writes); the *consecutive-failure* guard
/// (K) is what actually catches a stuck loop, so N can be loose.
/// Bumped 24 → 80 after the first dev-test tripped it mid-project-scaffold.
pub const DEFAULT_MAX_TURN_ITERATIONS: usize = 80;

// ---------------------------------------------------------------------------
// Data model (CLAUDE.md ④.a「数据模型」: Turn / Plan / Action / Observation)
// ---------------------------------------------------------------------------

/// Per-step lifecycle. A step flipping to `Done` ≈ a 小主题 closing (④.b L1).
/// Set by the MODEL via `SetPlan`, never by the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Failed,
}

/// One ordered milestone in the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub seq: usize,
    pub description: String,
    pub status: StepStatus,
}

impl PlanStep {
    pub fn new(seq: usize, description: impl Into<String>) -> Self {
        Self {
            seq,
            description: description.into(),
            status: StepStatus::Pending,
        }
    }
}

/// 完整清单：总目标 + 有序步骤。Not "next step only" — see CLAUDE.md ④.a for
/// why we keep the whole checklist (alignment with ④.b 小主题 + a progress
/// anchor that isn't a human watching). The model maintains step statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<PlanStep>,
}

impl Plan {
    /// Convenience: build a plan from a goal and step descriptions (all Pending).
    pub fn new(goal: impl Into<String>, steps: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let steps = steps
            .into_iter()
            .enumerate()
            .map(|(i, d)| PlanStep::new(i, d))
            .collect();
        Self {
            goal: goal.into(),
            steps,
        }
    }
}

/// A message the agent wants to send outward, described in ④-NEUTRAL terms —
/// `react.rs` deliberately does NOT import `AgentMessageKind` (③'s type). The
/// ③ boundary (`agent/turn_bridge.rs`) translates this into the typed message.
/// (CLAUDE.md ④.a「outward Action 选 A」.) Derives serde because it rides
/// inside the persistable [`TurnState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outbound {
    Broadcast { content: String },
    Answer { reply_to: String, content: String },
}

/// What the model decides to do this iteration. (CLAUDE.md ④.a: 四类动作.)
/// The outward kinds stay ③-neutral (`Outbound` / the `Wait` fields); the
/// runtime translates them to `AgentMessageKind` at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Declare or rewrite the checklist, including step statuses. The model's
    /// way of saying "here's my plan now / I marked step 2 done".
    SetPlan(Plan),
    /// Do one concrete operation. Routed through the [`ActionExecutor`] seam
    /// (④.d's future hook). Does NOT advance any step — the model does that.
    ToolCall { tool: String, args: String },
    /// Emit an outward, non-suspending message. Skeleton records it in
    /// `outgoing`; real wiring drains those to the dispatcher (③).
    Speak(Outbound),
    /// Suspend the turn pending an answer (maps to ASK_AGENT at the ③ edge).
    /// Carries the question to send so ③ can dispatch it before parking. The
    /// driver returns [`TurnOutcome::Suspended`]; ③ parks the agent in
    /// WAITING_ANSWER and re-enters when the answer arrives. (④.a 硬约束.)
    Wait {
        to: String,
        content: String,
        expected_format: Option<String>,
    },
    /// End the turn.
    Done { summary: String },
}

/// ④.c signal. The driver branches on THIS, never on the raw result — so
/// dropping in real ④.c logic later is "swap the judge", not "rewrite the
/// loop". (CLAUDE.md ④.a「④.c 插座」.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Continue,
    Retry,
    Escalate,
    Abort,
}

/// Result of executing an action + the ④.c judgment of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub raw: String,
    pub success: bool,
    pub verdict: Verdict,
}

// ---------------------------------------------------------------------------
// Suspendable turn state (CLAUDE.md ④.a 硬约束: Turn 可暂停 + Plan 可存取).
//
// Everything needed to pause mid-turn and resume later lives here and nowhere
// else. It derives Serialize/Deserialize because suspend→persist→resume is a
// hard requirement — the round-trip is one of the four tested paths. Persisting
// it is ⑥'s job; the *layer semantics* belong to ④.b. The driver only reads
// and writes this struct.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnState {
    /// The model-maintained checklist. `None` until the first `SetPlan`.
    pub plan: Option<Plan>,
    /// How many times the plan has been rewritten this turn.
    pub replan_count: usize,
    /// Total loop iterations so far (drives the N backstop).
    pub iterations: usize,
    /// Tool-call failures since the last success/replan (drives the K backstop).
    pub consecutive_failures: usize,
    /// The most recent observation, kept so a resumed loop has continuity.
    pub last_observation: Option<Observation>,
    /// Set by the runtime on resume — the answer that unblocked a `Wait`.
    /// The real [`ActionSource`] folds this into its context; skeleton fakes
    /// ignore it.
    pub pending_answer: Option<String>,
    /// Outward messages the loop wanted to send. Accumulated here (not sent
    /// inline) so the loop stays free of any ③ dependency; the runtime drains
    /// them to the dispatcher at the turn boundary.
    pub outgoing: Vec<Outbound>,
}

impl TurnState {
    /// Fresh state for a brand-new turn.
    pub fn fresh() -> Self {
        Self::default()
    }
}

/// How a turn ended. (CLAUDE.md ④.a 循环形状.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Agent committed `Done`; the runtime emits DONE → IDLE.
    Done { summary: String },
    /// Loop hit a `Wait`; the runtime dispatches this question, parks in
    /// WAITING_ANSWER, and will call `resume` when the matching answer arrives.
    Suspended {
        to: String,
        content: String,
        expected_format: Option<String>,
    },
    /// A hard backstop tripped (consecutive failures or loop budget). The
    /// runtime surfaces this to the user / PM rather than spinning.
    Escalated { reason: String },
}

// ---------------------------------------------------------------------------
// Seams. Each is the boundary to a block this file does NOT own.
// ---------------------------------------------------------------------------

/// The LLM seam (⑤ + ④.b). The one thing tests fake. Real impl owns its own
/// conversation/context (④.b) and calls the provider (⑤); the driver only asks
/// "what next?" and feeds back observations.
pub trait ActionSource {
    fn next_action(
        &mut self,
        state: &TurnState,
    ) -> impl std::future::Future<Output = Action> + Send;

    /// Told after every executed action so the source can fold the result
    /// into its context for the next decision. Default no-op (skeleton fakes
    /// don't track context; the LLM impl appends a `tool` message here).
    fn observe(&mut self, _obs: &Observation) {}
}

/// The single chokepoint every side-effecting action passes through —
/// ④.d's future hook (dry-run / temp workspace wrap goes HERE, not in the
/// loop). MVP impl executes directly. (CLAUDE.md ④.a「④.d 插座」.)
pub trait ActionExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> impl std::future::Future<Output = ExecResult> + Send;
}

/// Raw result of one action execution, before judgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub raw: String,
    pub success: bool,
}

/// ④.c's future hook. MVP stub ([`AlwaysContinue`]) trusts success blindly;
/// real ④.c inspects whether the result actually advanced the plan.
pub trait ResultJudge {
    fn judge(&mut self, state: &TurnState, raw: &str, success: bool) -> Verdict;
}

/// MVP ④.c stub: success → Continue, failure → Retry. Nothing smarter yet.
pub struct AlwaysContinue;

impl ResultJudge for AlwaysContinue {
    fn judge(&mut self, _state: &TurnState, _raw: &str, success: bool) -> Verdict {
        if success {
            Verdict::Continue
        } else {
            Verdict::Retry
        }
    }
}

/// Observability seam (CLAUDE.md ④.a「可观测性」). The driver records
/// milestone-level events here; real wiring forwards them onto a NEW ② event
/// type (UI-only side channel, same treatment as MCP_CALL_EVENT). RED LINE:
/// whatever consumes these MUST NOT loop them back into any agent's LLM
/// context — they are display/persistence only.
pub trait ProgressSink {
    fn record(&mut self, ev: ProgressEvent);
}

/// Milestone-level (选 A, not per-iteration/per-token) — see CLAUDE.md ④.a.
/// Plan events carry the full checklist so the UI can render "step 2 of 3,
/// in progress" straight from the model-owned statuses; action events are the
/// loop's own activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    PlanCreated { goal: String, steps: Vec<PlanStep> },
    PlanRevised { revision: usize, goal: String, steps: Vec<PlanStep> },
    ActionStarted { tool: String },
    ActionResult { ok: bool },
    Retrying { consecutive: usize },
    Escalated { reason: String },
    Suspended { waiting_for: String },
    TurnDone { summary: String },
}

/// Drops every event. Default for headless / not-yet-wired callers.
pub struct NullSink;

impl ProgressSink for NullSink {
    fn record(&mut self, _ev: ProgressEvent) {}
}

// ---------------------------------------------------------------------------
// The driver. Generic over the four seams so it's fully monomorphized and the
// LLM never enters the picture in tests.
// ---------------------------------------------------------------------------

pub struct PlanLoop<A, E, J, P> {
    pub source: A,
    pub executor: E,
    pub judge: J,
    pub progress: P,
    pub max_consecutive_failures: usize,
    pub max_turn_iterations: usize,
}

impl<A, E, J, P> PlanLoop<A, E, J, P>
where
    A: ActionSource,
    E: ActionExecutor,
    J: ResultJudge,
    P: ProgressSink,
{
    /// Construct with the default backstop tunables.
    pub fn new(source: A, executor: E, judge: J, progress: P) -> Self {
        Self {
            source,
            executor,
            judge,
            progress,
            max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
            max_turn_iterations: DEFAULT_MAX_TURN_ITERATIONS,
        }
    }

    /// Run a fresh turn to its outcome.
    pub async fn run_turn(&mut self) -> (TurnOutcome, TurnState) {
        let mut state = TurnState::fresh();
        let outcome = self.drive(&mut state).await;
        (outcome, state)
    }

    /// Resume a suspended turn. `answer` is the reply that unblocked the
    /// `Wait`; it's stashed in the state for the [`ActionSource`] to consume.
    /// `state` is whatever was persisted at suspend time — the round-trip
    /// through ⑥ is transparent to the driver.
    pub async fn resume(&mut self, mut state: TurnState, answer: String) -> (TurnOutcome, TurnState) {
        state.pending_answer = Some(answer);
        let outcome = self.drive(&mut state).await;
        (outcome, state)
    }

    /// The loop itself. Both `run_turn` and `resume` funnel here. It mutates
    /// `state` in place so that on a `Suspended` return the caller holds the
    /// exact state to persist.
    async fn drive(&mut self, state: &mut TurnState) -> TurnOutcome {
        loop {
            state.iterations += 1;
            if state.iterations > self.max_turn_iterations {
                let reason = format!(
                    "loop budget exhausted ({} iterations without Done)",
                    self.max_turn_iterations
                );
                self.progress.record(ProgressEvent::Escalated {
                    reason: reason.clone(),
                });
                return TurnOutcome::Escalated { reason };
            }

            match self.source.next_action(state).await {
                Action::SetPlan(plan) => {
                    // Model (re)declares the checklist. A replan is a fresh
                    // approach, so the consecutive-failure tally resets.
                    if state.plan.is_some() {
                        state.replan_count += 1;
                        self.progress.record(ProgressEvent::PlanRevised {
                            revision: state.replan_count,
                            goal: plan.goal.clone(),
                            steps: plan.steps.clone(),
                        });
                    } else {
                        self.progress.record(ProgressEvent::PlanCreated {
                            goal: plan.goal.clone(),
                            steps: plan.steps.clone(),
                        });
                    }
                    state.plan = Some(plan);
                    state.consecutive_failures = 0;
                }

                Action::ToolCall { tool, args } => {
                    self.progress.record(ProgressEvent::ActionStarted { tool: tool.clone() });

                    // ④.d hook: today a direct call; later a dry-run/commit wrap.
                    let result = self.executor.execute(&tool, &args).await;
                    // ④.c hook: branch only on the verdict, never on `result.raw`.
                    let verdict = self.judge.judge(state, &result.raw, result.success);
                    let obs = Observation {
                        raw: result.raw,
                        success: result.success,
                        verdict,
                    };
                    self.source.observe(&obs);
                    self.progress.record(ProgressEvent::ActionResult { ok: obs.success });
                    state.last_observation = Some(obs);

                    match verdict {
                        Verdict::Continue => {
                            state.consecutive_failures = 0;
                        }
                        Verdict::Retry => {
                            state.consecutive_failures += 1;
                            let n = state.consecutive_failures;
                            self.progress.record(ProgressEvent::Retrying { consecutive: n });
                            if n >= self.max_consecutive_failures {
                                let reason = format!(
                                    "{n} consecutive tool-call failures (limit {})",
                                    self.max_consecutive_failures
                                );
                                self.progress.record(ProgressEvent::Escalated {
                                    reason: reason.clone(),
                                });
                                return TurnOutcome::Escalated { reason };
                            }
                            // Stay in the loop; next `next_action` decides
                            // whether to retry differently or replan (B trigger).
                        }
                        Verdict::Escalate => {
                            let reason = "judge escalated".to_string();
                            self.progress.record(ProgressEvent::Escalated {
                                reason: reason.clone(),
                            });
                            return TurnOutcome::Escalated { reason };
                        }
                        Verdict::Abort => {
                            let summary = "aborted".to_string();
                            self.progress.record(ProgressEvent::TurnDone {
                                summary: summary.clone(),
                            });
                            return TurnOutcome::Done { summary };
                        }
                    }
                }

                Action::Speak(out) => {
                    // Collect; the runtime drains these to the dispatcher (③).
                    state.outgoing.push(out);
                }

                Action::Wait {
                    to,
                    content,
                    expected_format,
                } => {
                    self.progress.record(ProgressEvent::Suspended {
                        waiting_for: to.clone(),
                    });
                    return TurnOutcome::Suspended {
                        to,
                        content,
                        expected_format,
                    };
                }

                Action::Done { summary } => {
                    self.progress.record(ProgressEvent::TurnDone {
                        summary: summary.clone(),
                    });
                    return TurnOutcome::Done { summary };
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the four critical paths from CLAUDE.md ④.a「测试策略」, driven by
// scripted fakes so no real LLM is ever called.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake [`ActionSource`]: pops a pre-scripted action each iteration. The
    /// internal cursor persists across `run_turn`/`resume` calls on the same
    /// instance — modelling a source whose context survives the suspend.
    struct ScriptSource {
        actions: Vec<Action>,
        cursor: usize,
    }

    impl ScriptSource {
        fn new(actions: Vec<Action>) -> Self {
            Self { actions, cursor: 0 }
        }
    }

    impl ActionSource for ScriptSource {
        async fn next_action(&mut self, _state: &TurnState) -> Action {
            let a = self
                .actions
                .get(self.cursor)
                .cloned()
                .unwrap_or(Action::Done {
                    summary: "script exhausted".into(),
                });
            self.cursor += 1;
            a
        }
    }

    /// Fake [`ActionExecutor`]: returns scripted (success, body) pairs in
    /// order, clamping to the last entry once exhausted (so an "always fails"
    /// executor is just `vec![(false, ...)]`).
    struct ScriptExecutor {
        results: Vec<(bool, String)>,
        cursor: usize,
    }

    impl ScriptExecutor {
        fn new(results: Vec<(bool, &str)>) -> Self {
            Self {
                results: results.into_iter().map(|(ok, s)| (ok, s.to_string())).collect(),
                cursor: 0,
            }
        }

        fn always(ok: bool) -> Self {
            Self::new(vec![(ok, if ok { "ok" } else { "ERROR" })])
        }
    }

    impl ActionExecutor for ScriptExecutor {
        async fn execute(&mut self, _tool: &str, _args: &str) -> ExecResult {
            let idx = self.cursor.min(self.results.len().saturating_sub(1));
            let (success, raw) = self.results[idx].clone();
            self.cursor += 1;
            ExecResult { raw, success }
        }
    }

    /// Collecting [`ProgressSink`] so tests can assert the milestone stream.
    #[derive(Default)]
    struct VecSink(Vec<ProgressEvent>);

    impl ProgressSink for VecSink {
        fn record(&mut self, ev: ProgressEvent) {
            self.0.push(ev);
        }
    }

    fn tool() -> Action {
        Action::ToolCall {
            tool: "fs__write_file".into(),
            args: "{}".into(),
        }
    }

    // --- Path 1: happy path — plan, do the work, Done, no replan -----------
    #[tokio::test]
    async fn happy_path_succeeds_then_done() {
        let source = ScriptSource::new(vec![
            Action::SetPlan(Plan::new("build README", ["confirm", "write", "report"])),
            tool(),
            tool(),
            tool(),
            Action::Done {
                summary: "README created".into(),
            },
        ]);
        let mut lp = PlanLoop::new(source, ScriptExecutor::always(true), AlwaysContinue, VecSink::default());

        let (outcome, state) = lp.run_turn().await;

        assert_eq!(
            outcome,
            TurnOutcome::Done {
                summary: "README created".into()
            }
        );
        assert_eq!(state.replan_count, 0);
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.plan.is_some());
    }

    // --- Path 2: failure → replan → continue → Done ------------------------
    #[tokio::test]
    async fn failure_triggers_replan_then_succeeds() {
        let source = ScriptSource::new(vec![
            Action::SetPlan(Plan::new("goal", ["doomed step"])),
            tool(), // fails
            // model sees the failure and rewrites the plan (B trigger)
            Action::SetPlan(Plan::new("goal", ["better step"])),
            tool(), // succeeds
            Action::Done {
                summary: "recovered".into(),
            },
        ]);
        // First execute fails, the rest succeed.
        let executor = ScriptExecutor::new(vec![(false, "ERROR: boom"), (true, "ok")]);
        let mut lp = PlanLoop::new(source, executor, AlwaysContinue, VecSink::default());

        let (outcome, state) = lp.run_turn().await;

        assert_eq!(
            outcome,
            TurnOutcome::Done {
                summary: "recovered".into()
            }
        );
        assert_eq!(state.replan_count, 1, "exactly one replan happened");
        assert_eq!(state.consecutive_failures, 0, "replan reset the failure tally");
    }

    // --- Path 3: hard backstop — K consecutive failures → Escalated --------
    #[tokio::test]
    async fn backstop_escalates_after_k_consecutive_failures() {
        // Source keeps trying tools; executor always fails; no replan between.
        let source = ScriptSource::new(vec![
            Action::SetPlan(Plan::new("goal", ["flaky step"])),
            tool(),
            tool(),
            tool(),
            tool(),
        ]);
        let mut lp = PlanLoop::new(
            source,
            ScriptExecutor::always(false),
            AlwaysContinue,
            VecSink::default(),
        );
        lp.max_consecutive_failures = 2; // K = 2

        let (outcome, state) = lp.run_turn().await;

        match outcome {
            TurnOutcome::Escalated { reason } => {
                assert!(reason.contains("2 consecutive"), "got: {reason}");
            }
            other => panic!("expected Escalated, got {other:?}"),
        }
        assert_eq!(state.consecutive_failures, 2);
    }

    // --- Path 4: suspend → persist round-trip → resume → Done --------------
    #[tokio::test]
    async fn suspend_persist_resume_continues_from_saved_point() {
        let source = ScriptSource::new(vec![
            Action::SetPlan(Plan::new("ask then act", ["ask backend", "use answer"])),
            Action::Wait {
                to: "backend_dev".into(),
                content: "which auth scheme?".into(),
                expected_format: None,
            },
            // --- resume continues here, cursor preserved on the same source ---
            tool(),
            Action::Done {
                summary: "used the answer".into(),
            },
        ]);
        let mut lp = PlanLoop::new(source, ScriptExecutor::always(true), AlwaysContinue, VecSink::default());

        // Run until it suspends.
        let (outcome, state) = lp.run_turn().await;
        assert_eq!(
            outcome,
            TurnOutcome::Suspended {
                to: "backend_dev".into(),
                content: "which auth scheme?".into(),
                expected_format: None,
            }
        );
        assert!(state.plan.is_some(), "plan survives the suspend");

        // Persist → reload (the ⑥ round-trip the loop must tolerate).
        let json = serde_json::to_string(&state).unwrap();
        let restored: TurnState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, state);

        // Resume from the restored state with the answer.
        let (outcome, final_state) = lp.resume(restored, "use JWT".into()).await;
        assert_eq!(
            outcome,
            TurnOutcome::Done {
                summary: "used the answer".into()
            }
        );
        // Continuity: the iteration counter kept climbing across the suspend.
        assert!(final_state.iterations > state.iterations);
    }

    // --- Observability: milestone stream is emitted in order ---------------
    #[tokio::test]
    async fn emits_milestone_progress_in_order() {
        let source = ScriptSource::new(vec![
            Action::SetPlan(Plan::new("g", ["s0"])),
            tool(),
            Action::Done {
                summary: "ok".into(),
            },
        ]);
        let mut lp = PlanLoop::new(source, ScriptExecutor::always(true), AlwaysContinue, VecSink::default());
        let _ = lp.run_turn().await;

        let evs = lp.progress.0;
        assert!(matches!(evs[0], ProgressEvent::PlanCreated { .. }));
        assert!(matches!(evs[1], ProgressEvent::ActionStarted { .. }));
        assert!(matches!(evs[2], ProgressEvent::ActionResult { ok: true }));
        assert!(matches!(evs[3], ProgressEvent::TurnDone { .. }));
    }
}
