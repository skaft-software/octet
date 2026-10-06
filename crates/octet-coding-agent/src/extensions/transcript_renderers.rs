//! Bounded asynchronous transcript jobs. The renderer only reads immutable
//! snapshots; no extension code or RPC ever runs under its state lock.
use super::*;
use crate::tui::view::transcript_extensions::{Candidate, Key};
use octet_agent::extension_process::{
    TranscriptRenderContent, TranscriptRenderRequest, TranscriptRenderResponse,
    EXTENSION_FEATURE_TRANSCRIPT_RENDER,
};

const MAX_JOBS: usize = 4;
const MAX_OBSERVATIONS: usize = 128;
struct Completed {
    serial: u64,
    candidate: Candidate,
    owners: Vec<ExtensionResourceOwner>,
    owner: ExtensionResourceOwner,
    response: TranscriptRenderResponse,
}
struct Job {
    serial: u64,
    task: JoinHandle<()>,
}

pub(super) struct TranscriptRenderers {
    jobs: std::collections::HashMap<Key, Job>,
    tx: mpsc::Sender<Completed>,
    rx: mpsc::Receiver<Completed>,
    serial: u64,
    owners: Vec<ExtensionResourceOwner>,
    entries: VecDeque<(ExtensionResourceOwner, String, Value)>,
    invalidations: Vec<(ExtensionResourceOwner, Option<String>)>,
}
impl Default for TranscriptRenderers {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(MAX_JOBS);
        Self {
            jobs: Default::default(),
            tx,
            rx,
            serial: 0,
            owners: Vec::new(),
            entries: VecDeque::new(),
            invalidations: Vec::new(),
        }
    }
}
impl Drop for TranscriptRenderers {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl TranscriptRenderers {
    pub(super) fn cancel(&mut self) {
        for (_, job) in self.jobs.drain() {
            job.task.abort();
        }
        while self.rx.try_recv().is_ok() {}
    }
    pub(super) fn committed(
        &mut self,
        owner: ExtensionResourceOwner,
        namespace: String,
        entry: Value,
    ) -> bool {
        if self.entries.len() == MAX_OBSERVATIONS {
            return false;
        }
        self.entries.push_back((owner, namespace, entry));
        true
    }
    pub(super) fn invalidate(&mut self, owner: ExtensionResourceOwner, source: Option<String>) {
        if let Some((_, pending)) = self
            .invalidations
            .iter_mut()
            .find(|(current, _)| current == &owner)
        {
            if pending != &source {
                *pending = None;
            }
        } else if self.invalidations.len() < MAX_OBSERVATIONS {
            self.invalidations.push((owner, source));
        }
    }
}
fn fallback() -> TranscriptRenderResponse {
    TranscriptRenderResponse {
        registered: false,
        lines: None,
        markdown: None,
        render_shell: None,
    }
}

impl ExecutableExtensions {
    pub(super) fn sync_transcript_renderers(&mut self, shell: &mut InteractiveShell) {
        let processes = if self.remote_ui_wake.is_some() {
            self.processes
                .iter()
                .filter(|process| {
                    process.is_running()
                        && process.supports_feature(EXTENSION_FEATURE_TRANSCRIPT_RENDER)
                })
                .filter_map(|process| {
                    let context =
                        extension_execution_context(process, self.resource_owner.as_deref());
                    context.resource_owner.as_ref()?;
                    Some((process.clone(), context))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let owners = processes
            .iter()
            .filter_map(|(_, context)| context.resource_owner.clone())
            .collect::<Vec<_>>();
        let driver = &mut self.transcript_renderers;
        if driver.owners != owners {
            driver.cancel();
            driver.owners = owners.clone();
        }
        shell.set_transcript_render_owners(owners.clone());
        while let Some((owner, namespace, entry)) = driver.entries.pop_front() {
            if owners.contains(&owner) {
                shell.append_private_transcript_entry(namespace, entry);
            }
        }
        for (owner, source) in driver.invalidations.drain(..) {
            if owners.contains(&owner) {
                // Cancel before dropping snapshots. A result already queued at
                // invalidation cannot acquire the replacement job's serial.
                for (_, job) in driver.jobs.drain() {
                    job.task.abort();
                }
                shell.invalidate_transcript_renderer(&owner, source.as_deref());
            }
        }
        while let Ok(completed) = driver.rx.try_recv() {
            if driver
                .jobs
                .get(&completed.candidate.key)
                .is_some_and(|job| job.serial == completed.serial)
            {
                driver.jobs.remove(&completed.candidate.key);
                if completed.owners == owners {
                    shell.accept_transcript_render(
                        &completed.candidate,
                        completed.owner,
                        completed.response,
                    );
                }
            }
        }
        driver.jobs.retain(|key, job| {
            if !shell.transcript_key_current(key) {
                job.task.abort();
                false
            } else {
                true
            }
        });
        if processes.is_empty() || driver.jobs.len() == MAX_JOBS {
            return;
        }
        let namespaces = processes
            .iter()
            .map(|(process, _)| process.descriptor().manifest.name.clone())
            .collect::<std::collections::HashSet<_>>();
        for candidate in shell.transcript_render_candidates_bounded(MAX_JOBS, Some(&namespaces)) {
            if driver.jobs.len() == MAX_JOBS {
                break;
            }
            if driver.jobs.contains_key(&candidate.key) {
                continue;
            }
            let selected = processes
                .iter()
                .filter(|(process, _)| {
                    candidate
                        .namespace
                        .as_ref()
                        .is_none_or(|namespace| namespace == &process.descriptor().manifest.name)
                })
                .cloned()
                .collect::<Vec<_>>();
            // A private entry is never disclosed to a different namespace.
            if selected.is_empty() {
                continue;
            }
            driver.serial = driver.serial.wrapping_add(1);
            let serial = driver.serial;
            let key = candidate.key.clone();
            let tx = driver.tx.clone();
            let wake = self.remote_ui_wake.clone();
            let owners = owners.clone();
            let owner = selected[0]
                .1
                .resource_owner
                .clone()
                .expect("selected owner");
            let task = tokio::spawn(async move {
                let work = async {
                    let mut response = fallback();
                    let mut rendered_by = owner.clone();
                    let mut render = candidate.render.clone();
                    for (process, context) in selected {
                        let request_owner = context.resource_owner.clone().expect("selected owner");
                        let request = TranscriptRenderRequest {
                            source_id: candidate.source_id.clone(),
                            width: candidate.key.width,
                            render: render.clone(),
                            context,
                        };
                        let Ok(next) = process.render_transcript(request).await else {
                            continue;
                        };
                        if !next.registered {
                            continue;
                        }
                        rendered_by = request_owner;
                        if let TranscriptRenderContent::Markdown { text, .. } = &mut render {
                            if let Some(markdown) = &next.markdown {
                                *text = markdown.clone();
                            }
                            response = next;
                        } else {
                            return (rendered_by, next);
                        }
                    }
                    (rendered_by, response)
                };
                let (owner, response) = tokio::time::timeout(RENDERER_RPC_DEADLINE, work)
                    .await
                    .unwrap_or_else(|_| (owner.clone(), fallback()));
                let _ = tx
                    .send(Completed {
                        serial,
                        candidate,
                        owners,
                        owner,
                        response,
                    })
                    .await;
                if let Some(wake) = wake {
                    wake.notify_one();
                }
            });
            driver.jobs.insert(key, Job { serial, task });
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
