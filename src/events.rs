//! Kubernetes Events for `CalibanTask`s (#56): so a stuck task is diagnosable
//! with `kubectl describe`, not only from the operator's own logs.

use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Recorder};

use crate::crd::Phase;
use crate::error::Error;

/// Kubernetes rejects an event note larger than 1 kB.
pub(crate) const NOTE_MAX: usize = 1024;

/// Cap a note at [`NOTE_MAX`] bytes without splitting a UTF-8 character.
pub(crate) fn truncate_note(s: &str) -> String {
    if s.len() <= NOTE_MAX {
        return s.to_string();
    }
    let mut end = NOTE_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn event(type_: EventType, reason: &str, action: &str, note: &str) -> Event {
    Event {
        type_,
        reason: reason.to_string(),
        note: Some(truncate_note(note)),
        action: action.to_string(),
        secondary: None,
    }
}

/// A Normal `PhaseChanged` event when the task's phase moves, e.g.
/// `Provisioning → Running`; `None` when it doesn't.
pub fn phase_changed(from: Option<Phase>, to: Phase) -> Option<Event> {
    (from != Some(to)).then(|| {
        let note = match from {
            Some(from) => format!("{from:?} → {to:?}"),
            None => format!("→ {to:?}"),
        };
        event(EventType::Normal, "PhaseChanged", "Reconcile", &note)
    })
}

/// A Warning for a task marked Failed, carrying the failure's own reason code
/// (`WorkspaceUnresolved`, `InvalidName`) and message.
pub fn task_failed(reason: &str, message: &str) -> Event {
    event(EventType::Warning, reason, "Admit", message)
}

/// A Warning for a reconcile that returned an error.
pub fn reconcile_error(err: &Error) -> Event {
    event(
        EventType::Warning,
        "ReconcileError",
        "Reconcile",
        &err.to_string(),
    )
}

/// Publish an event best-effort. A failure — including the chart not yet
/// granting `events.k8s.io` create/patch (helm-charts#53) — logs a warning and
/// never fails or delays the reconcile.
pub async fn publish(recorder: &Recorder, reference: &ObjectReference, ev: Event) {
    if let Err(e) = recorder.publish(&ev, reference).await {
        tracing::warn!(error = %e, reason = %ev.reason, "failed to publish event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::Phase;
    use kube::runtime::events::EventType;

    #[test]
    fn a_phase_transition_is_a_normal_event_naming_both_phases() {
        let ev = phase_changed(Some(Phase::Provisioning), Phase::Running).expect("a change");
        assert_eq!(ev.type_, EventType::Normal);
        assert_eq!(ev.reason, "PhaseChanged");
        let note = ev.note.unwrap();
        assert!(
            note.contains("Provisioning") && note.contains("Running"),
            "{note}"
        );
    }

    #[test]
    fn an_unchanged_phase_emits_nothing() {
        assert!(phase_changed(Some(Phase::Running), Phase::Running).is_none());
    }

    #[test]
    fn a_first_phase_is_reported_from_none() {
        let ev = phase_changed(None, Phase::Pending).expect("a first phase is a change");
        assert!(ev.note.unwrap().contains("Pending"));
    }

    #[test]
    fn a_task_failure_is_a_warning_carrying_its_reason() {
        let ev = task_failed("InvalidName", "too long");
        assert_eq!(ev.type_, EventType::Warning);
        assert_eq!(ev.reason, "InvalidName");
        assert_eq!(ev.note.as_deref(), Some("too long"));
    }

    #[test]
    fn a_reconcile_error_is_a_warning() {
        let ev = reconcile_error(&crate::error::Error::MissingUid("t".into()));
        assert_eq!(ev.type_, EventType::Warning);
        assert_eq!(ev.reason, "ReconcileError");
        assert!(ev.note.unwrap().contains("uid"));
    }

    /// Kubernetes rejects an event note over 1 kB. Truncation must land on a
    /// UTF-8 character boundary, or slicing would panic.
    #[test]
    fn notes_are_capped_at_one_kilobyte_on_a_char_boundary() {
        let long = "é".repeat(1000); // 2000 bytes
        let t = truncate_note(&long);
        assert!(t.len() <= NOTE_MAX, "{}", t.len());
        assert!(long.starts_with(&t));
        assert_eq!(truncate_note("short"), "short");
    }
}
