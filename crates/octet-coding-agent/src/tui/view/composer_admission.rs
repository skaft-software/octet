//! A submitted remote draft is an input transaction, not a native editor write.
//!
//! Native command/queue admission consumes this projection through the same
//! drain methods as keyboard input. The recovery checkpoint remains untouched;
//! only the editor may clear its state after the disposition is acknowledged.
use super::*;
use std::rc::Rc;

#[derive(Default)]
pub(super) struct SubmittedDraft {
    text: String,
    consumed: bool,
}

pub(crate) struct ComposerAdmission {
    draft: Rc<RefCell<Option<SubmittedDraft>>>,
    complete: Option<Box<dyn FnOnce(bool)>>,
}

impl Drop for ComposerAdmission {
    fn drop(&mut self) {
        let draft = self
            .draft
            .borrow_mut()
            .take()
            .expect("active composer admission");
        self.complete.take().expect("single disposition")(draft.consumed);
    }
}

impl InteractiveShell {
    pub(crate) fn begin_composer_admission(
        &mut self,
        text: String,
        complete: impl FnOnce(bool) + 'static,
    ) -> ComposerAdmission {
        let mut draft = self.submitted_draft.borrow_mut();
        assert!(draft.is_none(), "composer admission is serialized");
        *draft = Some(SubmittedDraft {
            text,
            consumed: false,
        });
        ComposerAdmission {
            draft: Rc::clone(&self.submitted_draft),
            complete: Some(Box::new(complete)),
        }
    }

    pub(super) fn submitted_text(&self) -> Option<String> {
        self.submitted_draft
            .borrow()
            .as_ref()
            .map(|draft| draft.text.clone())
    }

    pub(super) fn consume_submitted_text(&self) -> Option<String> {
        self.submitted_draft.borrow_mut().as_mut().map(|draft| {
            draft.consumed = true;
            draft.text.clone()
        })
    }

    pub(super) fn reject_submitted_text(&self) -> bool {
        if let Some(draft) = self.submitted_draft.borrow_mut().as_mut() {
            draft.consumed = false;
            true
        } else {
            false
        }
    }
}
