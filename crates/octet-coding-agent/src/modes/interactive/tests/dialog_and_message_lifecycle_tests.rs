//! Message and dialog lifecycle brackets: one boundary per text turn across a provider
//! retry, verbatim delta forwarding, and one settled boundary per host dialog.
//! Separate because these assert the extension-facing notification contract rather
//! than the host's own behaviour.

use super::*;

use super::support::*;

#[derive(Default)]
struct RecordingLifecycle {
    events: std::cell::RefCell<Vec<String>>,
}

impl RecordingLifecycle {
    fn events(&self) -> Vec<String> {
        self.events.borrow().clone()
    }
}

impl MessageLifecycleSink for RecordingLifecycle {
    fn message_started(&mut self, message_id: &str) {
        self.events
            .borrow_mut()
            .push(format!("started:{message_id}"));
    }

    fn message_delta(&mut self, delta: &str) {
        self.events.borrow_mut().push(format!("delta:{delta}"));
    }

    fn message_settled(&mut self, message_id: &str) {
        self.events
            .borrow_mut()
            .push(format!("settled:{message_id}"));
    }
}

impl DialogLifecycleSink for RecordingLifecycle {
    fn open_dialog(&self, dialog: &str) {
        self.events.borrow_mut().push(format!("started:{dialog}"));
    }

    fn close_dialog(&self, dialog: &str) {
        self.events.borrow_mut().push(format!("settled:{dialog}"));
    }
}

fn completed_run() -> AgentEvent {
    AgentEvent::RunFinished {
        head: EntryId("entry-1".to_owned()),
        reason: octet_agent::FinishReason::Completed,
    }
}

#[test]
fn assistant_message_lifecycle_brackets_one_message_per_text_turn() {
    let mut lifecycle = AssistantMessageLifecycle::default();
    let mut sink = RecordingLifecycle::default();
    lifecycle.observe(&mut sink, &text_delta(""));
    assert!(sink.events().is_empty(), "an empty delta opens nothing");
    lifecycle.observe(&mut sink, &text_delta("Hel"));
    lifecycle.observe(
        &mut sink,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "hidden reasoning".to_owned(),
        },
    );
    lifecycle.observe(&mut sink, &text_delta("lo"));
    assert_eq!(
        sink.events(),
        ["started:assistant-1", "delta:Hel", "delta:lo"]
    );
    // Every terminal run outcome settles the open boundary exactly once.
    lifecycle.observe(&mut sink, &completed_run());
    lifecycle.observe(&mut sink, &completed_run());
    assert_eq!(sink.events().len(), 4);
    assert_eq!(
        sink.events().last().map(String::as_str),
        Some("settled:assistant-1")
    );
    // A later turn opens a fresh, still bounded identifier.
    lifecycle.observe(&mut sink, &text_delta("again"));
    assert_eq!(
        sink.events().last().map(String::as_str),
        Some("delta:again")
    );
    assert!(sink.events().contains(&"started:assistant-2".to_owned()));
}

#[test]
fn assistant_message_lifecycle_keeps_one_boundary_across_provider_retry() {
    let mut lifecycle = AssistantMessageLifecycle::default();
    let mut sink = RecordingLifecycle::default();
    lifecycle.observe(&mut sink, &text_delta("partial"));
    lifecycle.observe(
        &mut sink,
        &AgentEvent::ProviderRetry {
            attempt: 1,
            max_attempts: 3,
            delay: Duration::from_millis(1),
            error: "stream reset".to_owned(),
        },
    );
    lifecycle.observe(&mut sink, &text_delta("final"));
    assert_eq!(
        sink.events(),
        ["started:assistant-1", "delta:partial", "delta:final"]
    );
    lifecycle.settle(&mut sink);
    assert_eq!(
        sink.events().last().map(String::as_str),
        Some("settled:assistant-1")
    );
}

#[test]
fn assistant_message_lifecycle_forwards_each_delta_verbatim() {
    let mut lifecycle = AssistantMessageLifecycle::default();
    let mut sink = RecordingLifecycle::default();
    for index in 0..64 {
        lifecycle.observe(&mut sink, &text_delta(&format!("t{index}")));
    }
    let events = sink.events();
    assert_eq!(events[0], "started:assistant-1");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("delta:"))
            .count(),
        64,
        "the producer forwards every increment; the host owns coalescing"
    );
    assert!(
        !events.iter().any(|event| event.starts_with("settled:")),
        "no notification per delta is issued before the terminal boundary"
    );
}

#[tokio::test]
async fn present_host_dialog_settles_once_on_every_outcome() {
    let sink = RecordingLifecycle::default();
    let value = present_host_dialog(&sink, "confirm", async { Ok::<_, anyhow::Error>(true) })
        .await
        .expect("approved dialog");
    assert!(value);
    assert_eq!(sink.events(), ["started:confirm", "settled:confirm"]);
    let refused = present_host_dialog(&sink, "input", async {
        Err::<Option<String>, _>(anyhow::anyhow!("dismissed"))
    })
    .await;
    assert!(refused.is_err());
    assert_eq!(
        sink.events(),
        [
            "started:confirm",
            "settled:confirm",
            "started:input",
            "settled:input",
        ]
    );
}
