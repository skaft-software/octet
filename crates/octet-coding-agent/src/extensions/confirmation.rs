//! The confirmation handler extensions ask before side effects.

use super::*;

/// One char-safe bounded slice of a user shell command for a fixed-shape
/// extension notification. The bound is the same one the host applies, so a
/// long escape never turns into a protocol failure diagnostic.
pub(super) fn bounded_notification_command(command: &str) -> &str {
    let mut end = command.len().min(MAX_EXTENSION_BASH_COMMAND_BYTES);
    while end > 0 && !command.is_char_boundary(end) {
        end -= 1;
    }
    &command[..end]
}

pub trait ExtensionConfirmationHandler {
    /// Wait until the frontend asks to cancel the in-flight command. Dropping
    /// this future must leave the input source usable by `confirm`.
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(std::future::pending())
    }

    /// Interactive commands lend the same shell/input owner to the fleet drain.
    /// Other frontends retain the bounded cancellation-only behavior.
    fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
        None
    }

    fn wait_for_command_event<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Event>>> + 'a>> {
        Box::pin(async move {
            self.wait_for_cancel().await?;
            Ok(None)
        })
    }

    /// Apply an unfocused command-loop event. True requests cancellation.
    fn command_event(&mut self, _event: Event) -> bool {
        false
    }

    /// Host-owned command cancellation keys are checked before focused remote UI
    /// input, so a long attended command remains cancellable while mounted.
    fn command_cancellation_event(&mut self, _event: &Event) -> bool {
        false
    }

    /// The current attended command admitted a fullscreen mount that owns the
    /// interactive shell instead of its parent command menu.
    fn should_yield_to_fullscreen(&self) -> bool {
        false
    }

    /// Receive one bounded, request-scoped extension command progress event.
    ///
    /// Implementations must treat this as transient presentation only; it is
    /// never a command result or durable session content.
    fn progress(&mut self, _extension: &str, _progress: &ToolProgress) {}

    /// Clear transient command progress after its request settles.
    fn finish_progress(&mut self, _extension: &str) {}

    /// Native exact-effect confirmations must never consume action-level preapproval.
    fn confirm_effect<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a octet_agent::tool::ToolConfirmation,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        Box::pin(async move {
            let prompt = ConfirmationRequest {
                parent_request_id: None,
                prompt: request.prompt.clone(),
                detail: request.detail.clone(),
                destructive: request.destructive,
                default: request.default,
            };
            self.confirm(extension, &prompt).await
        })
    }

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>>;

    fn input<'a>(
        &'a mut self,
        _extension: &'a str,
        _request: &'a ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

#[cfg_attr(not(test), allow(dead_code))] // used by tests only
pub(super) struct PreapprovedExtensionConfirmation<'a, H: ?Sized> {
    // One action-level approval may satisfy only the first confirmation emitted
    // by that same manifest-scoped command; later prompts still reach the UI.
    pub(super) inner: &'a mut H,
    pub(super) remaining: usize,
}

impl<H> ExtensionConfirmationHandler for PreapprovedExtensionConfirmation<'_, H>
where
    H: ExtensionConfirmationHandler + ?Sized,
{
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        self.inner.wait_for_cancel()
    }

    fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
        self.inner.command_shell()
    }
    fn wait_for_command_event<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Event>>> + 'a>> {
        self.inner.wait_for_command_event()
    }
    fn command_event(&mut self, event: Event) -> bool {
        self.inner.command_event(event)
    }
    fn command_cancellation_event(&mut self, event: &Event) -> bool {
        self.inner.command_cancellation_event(event)
    }

    fn should_yield_to_fullscreen(&self) -> bool {
        self.inner.should_yield_to_fullscreen()
    }

    fn confirm_effect<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a octet_agent::tool::ToolConfirmation,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        self.inner.confirm_effect(extension, request)
    }

    fn progress(&mut self, extension: &str, progress: &ToolProgress) {
        self.inner.progress(extension, progress);
    }

    fn finish_progress(&mut self, extension: &str) {
        self.inner.finish_progress(extension);
    }

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        if self.remaining > 0 {
            self.remaining -= 1;
            Box::pin(std::future::ready(Ok(true)))
        } else {
            self.inner.confirm(extension, request)
        }
    }

    fn input<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        self.inner.input(extension, request)
    }
}
