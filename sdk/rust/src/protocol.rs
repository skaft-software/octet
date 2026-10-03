//! Feature-negotiated API 0.4 only. This is intentionally not canonical 0.3.
use crate::{schema, CallContext, Error, Extension, Terminal, ToolResult, MAX_FRAME_BYTES};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FEATURES: [&str; 2] = ["request_cancellation", "content_parts"];
const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
static STDIO_USED: AtomicBool = AtomicBool::new(false);
type Writer = Arc<Mutex<std::io::Stdout>>;
struct Active {
    id: Value,
    context: CallContext,
    thread: JoinHandle<Result<(), Error>>,
}

pub(crate) fn run(extension: Extension) -> Result<(), Error> {
    if STDIO_USED.swap(true, Ordering::AcqRel) {
        return Err(Error::rpc(
            -32600,
            "stdio runtime may run only once per process",
        ));
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("octet-native-reader".into())
        .spawn(move || {
            let mut input = BufReader::new(std::io::stdin());
            loop {
                let frame = read_frame(&mut input);
                let done = !matches!(frame, Ok(Some(_)));
                if sender.send(frame).is_err() || done {
                    break;
                }
            }
        })
        .map_err(|_| Error::internal())?;
    let writer = Arc::new(Mutex::new(std::io::stdout()));
    let mut active = None;
    let mut shutdown_id = None;
    let result = serve(
        &extension,
        &receiver,
        &writer,
        &mut active,
        &mut shutdown_id,
    );
    // Never return to a C/C++ author while a callback still borrows their data.
    drain(&mut active)?;
    result?;
    if let Some(id) = shutdown_id {
        send(&writer, success(id, json!({})))?;
    }
    Ok(())
}

fn serve(
    extension: &Extension,
    receiver: &mpsc::Receiver<Result<Option<Vec<u8>>, Error>>,
    writer: &Writer,
    active: &mut Option<Active>,
    shutdown_id: &mut Option<Value>,
) -> Result<(), Error> {
    let deadline = Instant::now() + INIT_TIMEOUT;
    let mut initialized = false;
    let mut bad_frames = 0;
    loop {
        reap(active)?;
        if !initialized && Instant::now() >= deadline {
            return Err(Error::rpc(-32000, "initialization deadline exceeded"));
        }
        let frame = match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(Err(error)) => {
                send(writer, failure(Value::Null, &error))?;
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        let request = match parse(&frame) {
            Ok(value) => value,
            Err(error) => {
                send(writer, failure(Value::Null, &error))?;
                bad_frames += 1;
                if bad_frames >= 8 {
                    return Err(Error::rpc(-32600, "malformed frame budget exhausted"));
                }
                continue;
            }
        };
        let object = request.as_object().unwrap();
        let id = object.get("id").cloned();
        let method = object["method"].as_str().unwrap();
        let params = &object["params"];
        // Completion may have happened while recv_timeout waited for this frame.
        reap(active)?;
        if active.as_ref().is_some_and(|a| id.as_ref() == Some(&a.id)) {
            let error = Error::rpc(-32600, "duplicate active request id; stream closed");
            // An error on the duplicated ID would create two terminals for the
            // original call. Use null and close the stream instead.
            send(writer, failure(Value::Null, &error))?;
            return Err(error);
        }
        if method == "$/cancelRequest" {
            let cancellation = validate_cancel(params, id.is_none());
            match cancellation {
                Ok(target) if initialized => {
                    if let Some(call) = active.as_ref().filter(|a| &a.id == target) {
                        call.context.cancel();
                    }
                }
                Ok(_) => {
                    eprintln!("octet-native: cancellation before initialization refused");
                }
                Err(error) => {
                    if let Some(id) = id {
                        send(writer, failure(id, &error))?;
                    } else {
                        eprintln!("octet-native: invalid cancellation notification refused");
                    }
                }
            }
            continue;
        }
        let Some(id) = id else {
            // Notifications cannot receive JSON-RPC replies. This runtime never
            // silently dispatches unsupported hooks/UI or tool notifications.
            eprintln!("octet-native: unsupported notification refused");
            continue;
        };
        if !initialized {
            if method != "initialize" {
                let error = Error::rpc(-32600, "initialize must be the first request");
                send(writer, failure(id, &error))?;
                return Err(error);
            }
            match initialize(extension, params) {
                Ok(result) => {
                    send(writer, success(id, result))?;
                    initialized = true;
                }
                Err(error) => {
                    send(writer, failure(id, &error))?;
                    return Err(error);
                }
            }
            continue;
        }
        match method {
            "initialize" => send(
                writer,
                failure(id, &Error::rpc(-32600, "already initialized")),
            )?,
            "shutdown" => {
                if !params.as_object().unwrap().is_empty() {
                    send(
                        writer,
                        failure(id, &Error::invalid("shutdown params must be empty")),
                    )?;
                } else {
                    *shutdown_id = Some(id);
                    return Ok(());
                }
            }
            "tool/call" => {
                // A just-completed worker may still be between flush and return.
                reap(active)?;
                if active.is_some() {
                    send(
                        writer,
                        failure(
                            id,
                            &Error::rpc(-32000, "native request concurrency exhausted"),
                        ),
                    )?;
                    continue;
                }
                let tool = match tool_call(extension, params) {
                    Ok(tool) => tool,
                    Err(error) => {
                        send(writer, failure(id, &error))?;
                        continue;
                    }
                };
                let context = CallContext {
                    terminal: Arc::new(Mutex::new(Terminal::default())),
                    host_context: params["context"].clone(),
                };
                let worker_context = context.clone();
                let arguments = params["arguments"].clone();
                let worker_id = id.clone();
                let writer = writer.clone();
                let thread = std::thread::Builder::new()
                    .name("octet-native-tool".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            worker_context.check_cancelled()?;
                            (tool.handler)(arguments, worker_context.clone())
                        }))
                        .unwrap_or_else(|_| Err(Error::internal()));
                        let result = match result {
                            Ok(result) => result.wire(),
                            Err(error) if error.code == 0 => {
                                ToolResult::error(error.message).wire()
                            }
                            Err(error) => Err(error),
                        };
                        let message = {
                            let mut terminal = worker_context.terminal.lock().unwrap();
                            terminal.settled = true;
                            if terminal.cancelled {
                                failure(worker_id, &Error::cancelled())
                            } else {
                                match result {
                                    Ok(result) => success(worker_id, result),
                                    Err(error) => failure(worker_id, &error),
                                }
                            }
                        };
                        send(&writer, message)
                    })
                    .map_err(|_| Error::internal())?;
                *active = Some(Active {
                    id,
                    context,
                    thread,
                });
            }
            _ => send(
                writer,
                failure(
                    id,
                    &Error::rpc(
                        -32601,
                        "Method not found: unsupported by native tool-only SDK",
                    ),
                ),
            )?,
        }
    }
}

fn reap(active: &mut Option<Active>) -> Result<(), Error> {
    // Terminal selection ends admission, not the final thread-return instruction.
    // A peer may send its next request immediately after reading the response.
    // Join the settled writer tail rather than spuriously rejecting that call.
    if active
        .as_ref()
        .is_some_and(|a| a.thread.is_finished() || a.context.terminal.lock().unwrap().settled)
    {
        active
            .take()
            .unwrap()
            .thread
            .join()
            .map_err(|_| Error::internal())??;
    }
    Ok(())
}
fn drain(active: &mut Option<Active>) -> Result<(), Error> {
    if let Some(call) = active.as_ref() {
        call.context.cancel();
    }
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while active.is_some() {
        reap(active)?;
        if active.is_none() {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!("octet-native: uncooperative handler exceeded 500 ms drain; exiting 70");
            std::process::exit(70);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn initialize(extension: &Extension, params: &Value) -> Result<Value, Error> {
    if params["api_version"] != "0.4"
        || params["protocol"]["version"] != "0.4"
        || params.get("contract").is_some()
    {
        return Err(Error::rpc(
            -32000,
            "native SDK requires exact feature-negotiated API 0.4",
        ));
    }
    for key in [
        "extension",
        "capabilities",
        "host",
        "contributes",
        "protocol",
    ] {
        if !params[key].is_object() {
            return Err(Error::invalid("initialize metadata must be objects"));
        }
    }
    if !params["workspace"]
        .as_str()
        .is_some_and(|s| s.len() <= 4096)
        || !params["octet_version"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128)
    {
        return Err(Error::invalid(
            "invalid initialize workspace or octet_version",
        ));
    }
    let contributes = params["contributes"].as_object().unwrap();
    let declared = names(&params["contributes"]["tools"])?;
    let registered = extension
        .tools
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared != registered {
        return Err(Error::invalid(
            "tool registrations must exactly match manifest declarations",
        ));
    }
    for (key, value) in contributes {
        match key.as_str() {
            "tools" => {}
            "commands" | "hooks" | "ui" | "tool_renderers" | "shortcuts" | "flags"
                if value.as_array().is_some_and(Vec::is_empty) => {}
            "context" | "notifications" | "confirmations" | "presentation" | "menu"
            | "providers"
                if value == &Value::Bool(false) => {}
            _ => {
                return Err(Error::invalid(
                    "unsupported manifest contribution: native SDK supports static tools only",
                ))
            }
        }
    }
    if params
        .get("flag_values")
        .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
    {
        return Err(Error::invalid("native SDK does not yet support CLI flags"));
    }
    let protocol = &params["protocol"];
    let required = names(&protocol["required_features"])?;
    let optional = names(&protocol["optional_features"])?;
    if !required.is_disjoint(&optional)
        || required.iter().any(|s| !FEATURES.contains(s))
        || FEATURES.iter().any(|s| !required.contains(s))
    {
        return Err(Error::rpc(
            -32000,
            "unsupported or missing required API 0.4 features",
        ));
    }
    if protocol["limits"]["max_concurrent_requests"]
        .as_u64()
        .is_none_or(|n| n == 0)
    {
        return Err(Error::invalid(
            "host concurrency offer must be a positive integer",
        ));
    }
    Ok(
        json!({"api_version":"0.4","tools":extension.catalog(),"commands":[],"protocol":{"version":"0.4","features":FEATURES,"limits":{"max_concurrent_requests":1}}}),
    )
}
fn names(value: &Value) -> Result<BTreeSet<&str>, Error> {
    let array = value
        .as_array()
        .filter(|a| a.len() <= 256)
        .ok_or_else(|| Error::invalid("name list must be a bounded array"))?;
    let mut names = BTreeSet::new();
    for value in array {
        let name = value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or_else(|| Error::invalid("invalid name in list"))?;
        if !names.insert(name) {
            return Err(Error::invalid("duplicate name in list"));
        }
    }
    Ok(names)
}
fn tool_call(extension: &Extension, params: &Value) -> Result<Arc<crate::Tool>, Error> {
    if params
        .as_object()
        .unwrap()
        .keys()
        .any(|s| !["name", "arguments", "context"].contains(&s.as_str()))
        || !params["arguments"].is_object()
        || !params["context"].is_object()
    {
        return Err(Error::invalid("invalid static tool/call parameters"));
    }
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::invalid("tool name must be a string"))?;
    let tool = extension
        .tools
        .get(name)
        .ok_or_else(|| Error::rpc(-32601, "unknown tool"))?;
    schema::arguments(&tool.definition["parameters"], &params["arguments"])?;
    Ok(tool.clone())
}
fn validate_cancel(params: &Value, notification: bool) -> Result<&Value, Error> {
    let object = params.as_object().unwrap();
    if !notification
        || object
            .keys()
            .any(|k| !["id", "reason"].contains(&k.as_str()))
        || !valid_id(&params["id"])
        || params
            .get("reason")
            .is_some_and(|v| !v.as_str().is_some_and(|s| s.len() <= 4096))
    {
        return Err(Error::invalid("invalid cancellation notification"));
    }
    Ok(&params["id"])
}
fn valid_id(id: &Value) -> bool {
    id.as_u64().is_some() || id.as_str().is_some_and(|s| s.len() <= 256)
}
fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn failure(id: Value, error: &Error) -> Value {
    // Error text is SDK-owned and bounded; never serialize hostile input back.
    let message = if error.message.len() <= 4096 {
        error.message.as_str()
    } else {
        "Internal error"
    };
    json!({"jsonrpc":"2.0","id":id,"error":{"code":error.code,"message":message}})
}
fn send(writer: &Writer, message: Value) -> Result<(), Error> {
    let mut frame = serde_json::to_vec(&message).map_err(|_| Error::internal())?;
    if frame.len() > MAX_FRAME_BYTES {
        return Err(Error::internal());
    }
    frame.push(b'\n');
    let mut output = writer.lock().unwrap();
    output
        .write_all(&frame)
        .and_then(|_| output.flush())
        .map_err(|_| Error::rpc(-32000, "protocol output closed"))
}

fn read_frame(input: &mut impl BufRead) -> Result<Option<Vec<u8>>, Error> {
    let mut frame = Vec::new();
    loop {
        let buffer = input
            .fill_buf()
            .map_err(|_| Error::rpc(-32000, "protocol input closed"))?;
        if buffer.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(Error::rpc(-32700, "unterminated JSONL frame"))
            };
        }
        let lf = buffer.iter().position(|b| *b == b'\n');
        let count = lf.unwrap_or(buffer.len());
        if count > MAX_FRAME_BYTES - frame.len() {
            return Err(Error::rpc(-32700, "JSONL frame exceeds 1 MiB"));
        }
        frame.extend_from_slice(&buffer[..count]);
        input.consume(count + usize::from(lf.is_some()));
        if lf.is_some() {
            return Ok(Some(frame));
        }
    }
}

fn parse(frame: &[u8]) -> Result<Value, Error> {
    let value = serde_json::from_slice::<UniqueValue>(frame)
        .map_err(|_| Error::rpc(-32700, "Parse error"))?
        .0;
    let mut nodes = 16_384;
    bounds(&value, 0, &mut nodes)?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::rpc(-32600, "Invalid Request"))?;
    if object
        .keys()
        .any(|k| !["jsonrpc", "id", "method", "params"].contains(&k.as_str()))
        || object.get("jsonrpc") != Some(&Value::String("2.0".into()))
        || !object
            .get("method")
            .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 128))
        || !object.get("params").is_some_and(Value::is_object)
        || object.get("id").is_some_and(|v| !valid_id(v))
    {
        return Err(Error::rpc(-32600, "Invalid Request"));
    }
    Ok(value)
}
fn bounds(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), Error> {
    if depth > 32 || *nodes == 0 {
        return Err(Error::invalid("JSON depth/node bounds exceeded"));
    }
    *nodes -= 1;
    match value {
        Value::Object(object) => {
            for child in object.values() {
                bounds(child, depth + 1, nodes)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                bounds(child, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Ordinary API 0.4 JSONL accepts whitespace, CRLF and arbitrary key ordering,
// but duplicate keys are ambiguous even on this noncanonical wire.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(UniqueVisitor)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON without duplicate keys")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(v.into()))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(v.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(v)
            .map(|v| UniqueValue(v.into()))
            .ok_or_else(|| E::custom("nonfinite number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(v.into()))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(v) = seq.next_element::<UniqueValue>()? {
            values.push(v.0);
        }
        Ok(UniqueValue(values.into()))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            if values.contains_key(&k) {
                return Err(serde::de::Error::custom("duplicate key"));
            }
            values.insert(k, map.next_value::<UniqueValue>()?.0);
        }
        Ok(UniqueValue(values.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_bounds_are_exact() {
        let exact = vec![b' '; MAX_FRAME_BYTES];
        let mut data = exact.clone();
        data.push(b'\n');
        assert_eq!(
            read_frame(&mut &data[..]).unwrap().unwrap().len(),
            MAX_FRAME_BYTES
        );
        data.insert(0, b' ');
        assert!(read_frame(&mut &data[..]).is_err());
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        assert!(read_frame(&mut &b""[..]).unwrap().is_none());
    }
    #[test]
    fn envelopes_are_validated_not_canonicalized() {
        assert!(parse(br#" {"params":{},"method":"shutdown","jsonrpc":"2.0","id":"s"} "#).is_ok());
        for data in [
            br#"{"jsonrpc":"2.0","method":"shutdown","params":{},"id":null}"#.as_slice(),
            br#"{"jsonrpc":"2.0","method":"shutdown","params":{},"id":true}"#,
            br#"{"jsonrpc":"2.0","jsonrpc":"2.0","method":"shutdown","params":{}}"#,
            br#"[]"#,
        ] {
            assert!(parse(data).is_err());
        }
    }
}
