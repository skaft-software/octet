//! Shared descriptor-bound append line for session records and invocation values.

use std::fs::File;
use std::io::{Seek, Write};
use std::sync::Mutex;

use fs2::FileExt;

use crate::session::SessionError;

#[derive(Debug)]
pub(crate) struct SessionWriter {
    pub(crate) state: Mutex<WriterState>,
    writable: bool,
    #[cfg(test)]
    fail_next_append: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
pub(crate) struct WriterState {
    file: File,
    pub(crate) len: u64,
    pub(crate) records: usize,
}

impl SessionWriter {
    pub(crate) fn new(file: File, len: u64, records: usize, writable: bool) -> Self {
        Self {
            state: Mutex::new(WriterState { file, len, records }),
            writable,
            #[cfg(test)]
            fail_next_append: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append(&self) {
        self.fail_next_append
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn persist(&self, bytes: &[u8]) -> Result<(), SessionError> {
        #[cfg(test)]
        if self
            .fail_next_append
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(SessionError::Io(std::io::Error::other(
                "injected append failure",
            )));
        }
        if !self.writable {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only session cannot persist records",
            )));
        }
        let append = || {
            let mut state = self.state.lock().map_err(|_| {
                SessionError::Io(std::io::Error::other("session mutation line poisoned"))
            })?;
            state.file.lock_exclusive()?;
            let result = (|| {
                if state.file.metadata()?.len() != state.len {
                    return Err(SessionError::ConcurrentModification);
                }
                let len = state
                    .len
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| SessionError::Limit("session file length overflow".into()))?;
                let records = state
                    .records
                    .checked_add(bytes.iter().filter(|b| **b == b'\n').count())
                    .ok_or_else(|| SessionError::Limit("session record count overflow".into()))?;
                if len > super::session::MAX_SESSION_FILE_BYTES
                    || records > super::session::MAX_SESSION_RECORDS
                {
                    return Err(SessionError::Limit(
                        "session append exceeds storage bound".into(),
                    ));
                }
                state.file.seek(std::io::SeekFrom::End(0))?;
                state.file.write_all(bytes)?;
                state.file.sync_data()?;
                state.len = len;
                state.records = records;
                Ok(())
            })();
            let unlocked = FileExt::unlock(&state.file);
            result?;
            unlocked?;
            Ok(())
        };
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            )
        }) {
            tokio::task::block_in_place(append)
        } else {
            append()
        }
    }
}
