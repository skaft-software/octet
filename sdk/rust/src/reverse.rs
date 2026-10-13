//! Bounded child requests on the runtime's existing JSON-RPC stream.
use crate::{protocol, CallContext, Error};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct Reverse {
    writer: protocol::Writer,
    pending: Mutex<Pending>,
}
#[derive(Default)]
struct Pending {
    next: u64,
    calls: BTreeMap<String, mpsc::SyncSender<Result<Value, Error>>>,
}
impl Reverse {
    pub fn new(writer: protocol::Writer) -> Self {
        Self {
            writer,
            pending: Mutex::new(Pending::default()),
        }
    }
    pub fn request(
        &self,
        call: &CallContext,
        parent: u64,
        method: &str,
        mut params: Value,
    ) -> Result<Value, Error> {
        let (tx, rx) = mpsc::sync_channel(1);
        let id = {
            let terminal = call.terminal.lock().unwrap();
            if terminal.cancelled {
                return Err(Error::cancelled());
            }
            if terminal.settled {
                return Err(Error::invalid("host services require an active call"));
            }
            let mut pending = self.pending.lock().unwrap();
            if pending.calls.len() >= 32 || pending.next >= 65_536 {
                return Err(Error::rpc(-32000, "reverse request quota exceeded"));
            }
            pending.next += 1;
            let id = format!("octet-native-{}", pending.next);
            pending.calls.insert(id.clone(), tx);
            params["parent_request_id"] = parent.into();
            if let Err(error) = protocol::send(
                &self.writer,
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            ) {
                pending.calls.remove(&id);
                return Err(error);
            }
            id
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(Error::internal()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if call.is_cancelled() {
                break Err(Error::cancelled());
            }
            if Instant::now() >= deadline {
                break Err(Error::rpc(-32000, "host service deadline exceeded"));
            }
        };
        if self.pending.lock().unwrap().calls.remove(&id).is_some() {
            protocol::send(
                &self.writer,
                json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id,"reason":"native caller stopped waiting"}}),
            )?;
        }
        result
    }
    pub fn cancel(&self, id: &Value) {
        if let Some(id) = id.as_str() {
            if let Some(sender) = self.pending.lock().unwrap().calls.remove(id) {
                let _ = sender.try_send(Err(Error::cancelled()));
            }
        }
    }

    /// The reader never waits on native code or a response consumer.
    pub fn response(&self, response: &Value) {
        let Some(id) = response["id"].as_str() else {
            return;
        };
        let Some(sender) = self.pending.lock().unwrap().calls.remove(id) else {
            return;
        };
        let result = if let Some(result) = response.get("result") {
            Ok(result.clone())
        } else {
            let error = &response["error"];
            Err(Error::rpc(
                error["code"].as_i64().unwrap() as i32,
                error["message"].as_str().unwrap(),
            ))
        };
        let _ = sender.try_send(result);
    }
}
