#![allow(dead_code)]

use crate::session_capture::types::SessionEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthStatus {
    Validated,
    Invalidated,
    Uncertain,
    Superseded,
}

impl TruthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TruthStatus::Validated => "validated",
            TruthStatus::Invalidated => "invalidated",
            TruthStatus::Uncertain => "uncertain",
            TruthStatus::Superseded => "superseded",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "validated" => TruthStatus::Validated,
            "invalidated" => TruthStatus::Invalidated,
            "superseded" => TruthStatus::Superseded,
            _ => TruthStatus::Uncertain,
        }
    }

    pub fn is_retrievable_default(&self) -> bool {
        !matches!(self, TruthStatus::Invalidated | TruthStatus::Superseded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthLifecycleSignal {
    ValidationEvidence,
    ContradictionEvidence,
    SupersessionEvidence,
    AmbiguousEvidence,
}

pub fn evolve_truth_status(current: TruthStatus, signal: TruthLifecycleSignal) -> TruthStatus {
    match signal {
        TruthLifecycleSignal::ValidationEvidence => match current {
            TruthStatus::Invalidated => TruthStatus::Uncertain,
            TruthStatus::Superseded => TruthStatus::Superseded,
            _ => TruthStatus::Validated,
        },
        TruthLifecycleSignal::ContradictionEvidence => TruthStatus::Invalidated,
        TruthLifecycleSignal::SupersessionEvidence => TruthStatus::Superseded,
        TruthLifecycleSignal::AmbiguousEvidence => match current {
            TruthStatus::Invalidated | TruthStatus::Superseded => current,
            _ => TruthStatus::Uncertain,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionPolicy {
    ContextSplit,
    ConfidenceDownshift,
    Supersession,
}

impl ContradictionPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContradictionPolicy::ContextSplit => "context_split",
            ContradictionPolicy::ConfidenceDownshift => "confidence_downshift",
            ContradictionPolicy::Supersession => "supersession",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalTuple {
    pub action: String,
    pub context: String,
    pub outcome: String,
    pub mechanism: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionResolution {
    pub contradiction_detected: bool,
    pub policy: ContradictionPolicy,
    pub status: TruthStatus,
    pub confidence: f32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalScoreBreakdown {
    pub relevance: f32,
    pub recency: f32,
    pub evidence_strength: f32,
    pub truth_status: f32,
    pub total: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStep {
    Intake,
    Plan,
    Execute,
    Verify,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionSignal {
    ClarifyIntent,
    AddConstraint,
    AskForEvidence,
    ResolveContradiction,
    Reprioritize,
    TightenVerification,
}

impl InterventionSignal {
    pub fn as_str(&self) -> &'static str {
        match self {
            InterventionSignal::ClarifyIntent => "clarify_intent",
            InterventionSignal::AddConstraint => "add_constraint",
            InterventionSignal::AskForEvidence => "ask_for_evidence",
            InterventionSignal::ResolveContradiction => "resolve_contradiction",
            InterventionSignal::Reprioritize => "reprioritize",
            InterventionSignal::TightenVerification => "tighten_verification",
        }
    }
}

pub fn map_intervention_signal(
    step: AgentStep,
    confidence: f32,
    contradiction: bool,
) -> InterventionSignal {
    if contradiction {
        return InterventionSignal::ResolveContradiction;
    }

    match step {
        AgentStep::Intake if confidence < 0.55 => InterventionSignal::ClarifyIntent,
        AgentStep::Plan if confidence < 0.55 => InterventionSignal::AddConstraint,
        AgentStep::Execute if confidence < 0.55 => InterventionSignal::Reprioritize,
        AgentStep::Verify if confidence < 0.7 => InterventionSignal::AskForEvidence,
        AgentStep::Complete if confidence < 0.7 => InterventionSignal::TightenVerification,
        _ => InterventionSignal::AskForEvidence,
    }
}

pub fn extract_causal_tuples(events: &[SessionEvent]) -> Vec<CausalTuple> {
    let mut tuples = Vec::new();
    let mut pending_action: Option<(String, Vec<String>)> = None;

    for event in events {
        match event {
            SessionEvent::ToolCall(call) => {
                let action = format!("tool:{}()", call.tool_name);
                let mut evidence = vec![format!("tool_call:{}", call.meta.raw_type)];
                if !call.arguments.is_null() {
                    evidence.push(format!(
                        "args:{}",
                        truncate_text(&call.arguments.to_string(), 140)
                    ));
                }
                pending_action = Some((action, evidence));
            }
            SessionEvent::ToolResult(result) => {
                if let Some((action, mut evidence)) = pending_action.take() {
                    let outcome = if result.success {
                        "success".to_string()
                    } else {
                        "failure".to_string()
                    };
                    let context = result
                        .meta
                        .project
                        .clone()
                        .or_else(|| result.meta.cwd.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    let mechanism = if result.success {
                        "tool_execution_verified".to_string()
                    } else {
                        "tool_execution_error".to_string()
                    };
                    evidence.push(format!(
                        "tool_result:{}",
                        truncate_text(&result.output, 160)
                    ));
                    let confidence = if result.success { 0.75 } else { 0.55 };
                    tuples.push(CausalTuple {
                        action,
                        context,
                        outcome,
                        mechanism,
                        confidence,
                        evidence,
                    });
                }
            }
            SessionEvent::UserMessage(msg) => {
                let text = msg.text.trim();
                if text.len() >= 12 {
                    tuples.push(CausalTuple {
                        action: "user_steering".to_string(),
                        context: msg
                            .meta
                            .project
                            .clone()
                            .or_else(|| msg.meta.cwd.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        outcome: "instruction_update".to_string(),
                        mechanism: "human_feedback_loop".to_string(),
                        confidence: 0.65,
                        evidence: vec![truncate_text(text, 180)],
                    });
                }
            }
            _ => {}
        }
    }

    tuples
}

pub fn detect_and_resolve_contradiction(
    current_status: TruthStatus,
    current_confidence: f32,
    existing_context: &str,
    incoming_context: &str,
    contradiction_signal: bool,
    supersession_signal: bool,
) -> ContradictionResolution {
    if supersession_signal {
        return ContradictionResolution {
            contradiction_detected: true,
            policy: ContradictionPolicy::Supersession,
            status: evolve_truth_status(current_status, TruthLifecycleSignal::SupersessionEvidence),
            confidence: current_confidence.min(0.6),
            note: "newer memory supersedes prior memory".to_string(),
        };
    }

    if !contradiction_signal {
        return ContradictionResolution {
            contradiction_detected: false,
            policy: ContradictionPolicy::ContextSplit,
            status: current_status,
            confidence: current_confidence,
            note: "no contradiction signal detected".to_string(),
        };
    }

    let same_context = normalize(existing_context) == normalize(incoming_context);
    if same_context {
        ContradictionResolution {
            contradiction_detected: true,
            policy: ContradictionPolicy::ConfidenceDownshift,
            status: evolve_truth_status(
                current_status,
                TruthLifecycleSignal::ContradictionEvidence,
            ),
            confidence: (current_confidence - 0.30).max(0.05),
            note: "same context conflict; downshift confidence and invalidate".to_string(),
        }
    } else {
        ContradictionResolution {
            contradiction_detected: true,
            policy: ContradictionPolicy::ContextSplit,
            status: evolve_truth_status(current_status, TruthLifecycleSignal::AmbiguousEvidence),
            confidence: (current_confidence - 0.15).max(0.05),
            note: "different contexts; split memory scopes".to_string(),
        }
    }
}

pub fn compute_retrieval_score(
    relevance: f32,
    recency: f32,
    evidence_strength: f32,
    truth_status: TruthStatus,
) -> RetrievalScoreBreakdown {
    let relevance = clamp01(relevance);
    let recency = clamp01(recency);
    let evidence_strength = clamp01(evidence_strength);
    let truth_factor = truth_weight(truth_status);
    let total =
        (0.45 * relevance) + (0.2 * recency) + (0.2 * evidence_strength) + (0.15 * truth_factor);

    RetrievalScoreBreakdown {
        relevance,
        recency,
        evidence_strength,
        truth_status: truth_factor,
        total: clamp01(total),
    }
}

pub fn should_exclude_from_default_retrieval(status: TruthStatus) -> bool {
    !status.is_retrievable_default()
}

fn normalize(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn truth_weight(status: TruthStatus) -> f32 {
    match status {
        TruthStatus::Validated => 1.0,
        TruthStatus::Uncertain => 0.5,
        TruthStatus::Invalidated => 0.0,
        TruthStatus::Superseded => 0.0,
    }
}

fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn truncate_text(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let truncated: String = raw.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

pub fn parse_json_list_column<T>(raw: &str) -> Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn to_json_string<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transitions_follow_signals() {
        assert_eq!(
            evolve_truth_status(
                TruthStatus::Uncertain,
                TruthLifecycleSignal::ValidationEvidence
            ),
            TruthStatus::Validated
        );
        assert_eq!(
            evolve_truth_status(
                TruthStatus::Validated,
                TruthLifecycleSignal::ContradictionEvidence
            ),
            TruthStatus::Invalidated
        );
        assert_eq!(
            evolve_truth_status(
                TruthStatus::Validated,
                TruthLifecycleSignal::SupersessionEvidence
            ),
            TruthStatus::Superseded
        );
        assert_eq!(
            evolve_truth_status(
                TruthStatus::Invalidated,
                TruthLifecycleSignal::ValidationEvidence
            ),
            TruthStatus::Uncertain
        );
    }

    #[test]
    fn contradiction_resolution_policies_are_applied() {
        let same_ctx = detect_and_resolve_contradiction(
            TruthStatus::Validated,
            0.8,
            "repo/a",
            "repo/a",
            true,
            false,
        );
        assert!(same_ctx.contradiction_detected);
        assert_eq!(same_ctx.policy, ContradictionPolicy::ConfidenceDownshift);
        assert_eq!(same_ctx.status, TruthStatus::Invalidated);
        assert!(same_ctx.confidence < 0.8);

        let split_ctx = detect_and_resolve_contradiction(
            TruthStatus::Validated,
            0.8,
            "repo/a",
            "repo/b",
            true,
            false,
        );
        assert_eq!(split_ctx.policy, ContradictionPolicy::ContextSplit);
        assert_eq!(split_ctx.status, TruthStatus::Uncertain);

        let superseded = detect_and_resolve_contradiction(
            TruthStatus::Validated,
            0.8,
            "repo/a",
            "repo/a",
            false,
            true,
        );
        assert_eq!(superseded.policy, ContradictionPolicy::Supersession);
        assert_eq!(superseded.status, TruthStatus::Superseded);
    }

    #[test]
    fn scoring_and_default_retrieval_filter_respect_truth_status() {
        let validated = compute_retrieval_score(0.9, 0.8, 0.7, TruthStatus::Validated);
        let invalidated = compute_retrieval_score(0.9, 0.8, 0.7, TruthStatus::Invalidated);
        assert!(validated.total > invalidated.total);
        assert!(!should_exclude_from_default_retrieval(
            TruthStatus::Validated
        ));
        assert!(should_exclude_from_default_retrieval(
            TruthStatus::Invalidated
        ));
        assert!(should_exclude_from_default_retrieval(
            TruthStatus::Superseded
        ));
    }
}
