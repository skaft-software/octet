// Handwritten adversarial API 0.4 peer, intentionally independent of SDK sugar.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Condvar, Mutex,
};

#[derive(Default)]
struct Circuit {
    count: u64,
}
#[derive(Default)]
struct State {
    output: Mutex<()>,
    responses: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    barriers: Mutex<HashMap<u64, Arc<(Mutex<bool>, Condvar)>>>,
    objects: Mutex<HashMap<String, Circuit>>,
    serial: AtomicU64,
    transport: Mutex<Value>,
}
impl State {
    fn send(&self, value: Value) {
        let _guard = self.output.lock().unwrap();
        println!("{value}");
        std::io::stdout().flush().unwrap();
    }
    fn log(&self, mut value: Value) {
        let _guard = self.output.lock().unwrap();
        value["pid"] = json!(std::process::id());
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open("calls.jsonl")
            .unwrap();
        f.write_all(format!("{value}\n").as_bytes()).unwrap();
    }
    fn notice(&self, value: Value) {
        self.send(
            json!({"jsonrpc":"2.0","method":"notification","params":{"message":value.to_string()}}),
        );
    }
    fn reverse(&self, parent: u64, method: &str, mut params: Value) -> Value {
        let id = format!("child-{}", self.serial.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel();
        self.responses.lock().unwrap().insert(id.clone(), tx);
        params["parent_request_id"] = json!(parent);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        rx.recv().unwrap()
    }
    fn bulk_request(&self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let response = self.reverse(id, method, params);
        if response.get("error").is_some() {
            return Err(response["error"]["data"]["code"]
                .as_str()
                .unwrap_or("blob_unavailable")
                .to_owned());
        }
        Ok(response["result"].clone())
    }
    fn bulk_call(&self, id: u64, name: &str, args: &Value) -> Result<(Value, bool, Value), String> {
        let double_payload = std::path::Path::new("bulk-double-payload").is_file();
        let payload = b"immutable-octet-bulk-v1".repeat(if double_payload { 4096 } else { 2048 });
        let mut metadata = json!({});
        let request = |method, params| self.bulk_request(id, method, params);
        match name {
            "bulk_create" => {
                let mut data = json!({});
                if args["mixed"] == true {
                    let resource = request("resource/register", json!({"type":"demo.Circuit"}))?;
                    self.objects.lock().unwrap().insert(
                        resource["$resource"].as_str().unwrap().into(),
                        Circuit::default(),
                    );
                    data["resource"] = resource;
                }
                let ticket = request(
                    "bulk/write",
                    json!({"profile":args["profile"].as_str().unwrap_or("local-file.v1"),"capacity":args["capacity"].as_u64().unwrap_or(payload.len() as u64),"media_type":"application/octet-stream"}),
                )?;
                let root = self.transport.lock().unwrap()["transfer_directory"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let path = std::path::Path::new(&root).join(ticket["locator"].as_str().unwrap());
                let mut scratch = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .unwrap();
                scratch.write_all(&payload).unwrap();
                if args["oversize"] == true {
                    scratch.write_all(b"X").unwrap();
                }
                scratch.flush().unwrap();
                let digest = if args["wrong_digest"] == true {
                    "0".repeat(64)
                } else if double_payload {
                    "d6a67e2c98975a9abd9f450005e8196710e5d94d3b9861ec6c259b682d45124d".into()
                } else {
                    "563d4ba65aa26dac08f929850a2f0fcf32209bb6c386e73e6e127ff2c1b91a77".into()
                };
                let blob = request(
                    "bulk/commit",
                    json!({"ticket":ticket["ticket"],"bytes":payload.len() as i64 + args["length_delta"].as_i64().unwrap_or(0),"digest":{"algorithm":"sha256","value":digest}}),
                )?;
                if args["rewrite"] == true {
                    scratch.seek(SeekFrom::Start(0)).unwrap();
                    scratch
                        .write_all(b"changed-original-after-host-snapshot")
                        .unwrap();
                    scratch.flush().unwrap();
                }
                drop(scratch);
                data["blob"] = blob.clone();
                let resources = if data.get("resource").is_some() {
                    vec![data["resource"].clone()]
                } else {
                    vec![]
                };
                self.notice(json!({"kind":"output_ready","request":id,"resources":resources,"blobs":[blob]}));
                if args["diagnostic"] == true || args["attachment_only"] == true {
                    metadata = json!({"octet_diagnostics_v1":[{"severity":"info","code":"bulk.fixture","message":"bounded scientific summary","attachments":[{"kind":"blob","id":blob["$blob"]}]}]});
                }
                if args["attachment_only"] == true {
                    data.as_object_mut().unwrap().remove("blob");
                }
                if args["bad_blob"] == true {
                    data["blob"]["$blob"] = json!("fabricated");
                }
                if args["bad_resource"] == true {
                    data["resource"]["$resource"] = json!("fabricated");
                }
                if args["invalid"] == true {
                    data["unexpected"] = json!(true);
                }
                if args["invalid_diagnostic"] == true {
                    metadata = json!({"octet_diagnostics_v1":[{"severity":"nope","code":"bad","message":"bad"}]});
                }
                if args["locator_leak"] == true {
                    data["leak"] = ticket["locator"].clone();
                }
                let error = args["error"] == true;
                Ok((if error { Value::Null } else { data }, error, metadata))
            }
            "bulk_read" => {
                let lease = request(
                    "bulk/read",
                    json!({"profile":args["profile"].as_str().unwrap_or("local-file.v1"),"blob":args["blob"]}),
                )?;
                let root = self.transport.lock().unwrap()["transfer_directory"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let data = std::fs::read(
                    std::path::Path::new(&root).join(lease["locator"].as_str().unwrap()),
                )
                .unwrap();
                self.notice(json!({"kind":"lease","request":id,"lease":lease["lease"]}));
                if args["keep"] != true {
                    request("bulk/release", json!({"id":lease["lease"]}))?;
                }
                Ok((
                    json!({"bytes":data.len(),"verified":data == payload}),
                    false,
                    metadata,
                ))
            }
            "bulk_release" => Ok((
                request("bulk/release", json!({"id":args["id"]}))?,
                false,
                metadata,
            )),
            _ => unreachable!(),
        }
    }
    fn call(self: Arc<Self>, message: Value) {
        let id = message["id"].as_u64().unwrap();
        let args = &message["params"]["arguments"];
        let name = message["params"]["name"].as_str().unwrap();
        let barrier = Arc::new((Mutex::new(false), Condvar::new()));
        self.barriers
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&barrier));
        self.log(json!({"kind":"call","name":name,"request":id}));
        self.notice(json!({"kind":"entered","name":name,"request":id}));
        let mut data = json!({});
        let mut error = false;
        let mut metadata = json!({});
        match name {
            "bulk_create" | "bulk_read" | "bulk_release" => match self.bulk_call(id, name, args) {
                Ok(result) => {
                    (data, error, metadata) = result;
                }
                Err(code) => {
                    data = Value::Null;
                    error = true;
                    self.notice(json!({"kind":"bulk_error","request":id,"code":code}));
                }
            },
            "create" => {
                let mut refs = Vec::new();
                for _ in 0..args["registrations"].as_u64().unwrap_or(1) {
                    let response =
                        self.reverse(id, "resource/register", json!({"type":"demo.Circuit"}));
                    if response.get("error").is_some() {
                        data = Value::Null;
                        error = true;
                        self.notice(json!({"kind":"registration_error","request":id,"error":response["error"]}));
                        break;
                    }
                    let reference = response["result"].clone();
                    self.objects.lock().unwrap().insert(
                        reference["$resource"].as_str().unwrap().to_owned(),
                        Circuit::default(),
                    );
                    refs.push(reference);
                }
                if !error {
                    data = json!({"resource":refs[0]});
                    if refs.len() > 1 {
                        data["second"] = refs[1].clone();
                    }
                    self.notice(json!({"kind":"output_ready","request":id,"resources":refs}));
                }
                if args["invalid"] == true {
                    data["unexpected"] = json!(true);
                }
                if args["error"] == true {
                    data = Value::Null;
                    error = true;
                }
            }
            "use" => {
                let mut objects = self.objects.lock().unwrap();
                let circuit = objects
                    .get_mut(args["resource"]["$resource"].as_str().unwrap())
                    .unwrap();
                circuit.count += 1;
                data = json!({"count":circuit.count});
            }
            "release" => {
                let result =
                    self.reverse(id, "resource/release", json!({"resource":args["resource"]}));
                error = result.get("error").is_some();
                data = Value::Null;
                self.notice(json!({"kind":"released","response":result}));
            }
            _ => {}
        }
        if args["block"] == true {
            let (lock, cv) = &*barrier;
            let mut allowed = lock.lock().unwrap();
            while !*allowed {
                allowed = cv.wait(allowed).unwrap();
            }
        }
        if args["rpc_error"] == true {
            self.send(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"domain failed"}}),
            );
        } else {
            let text = if error {
                "failed".into()
            } else if data.get("blob").is_some() {
                format!("immutable blob: {}", data["blob"])
            } else {
                "done".into()
            };
            let mut result = json!({"content":[{"type":"text","text":text}],"is_error":error});
            if metadata != json!({}) {
                result["metadata"] = metadata;
            }
            if !data.is_null() {
                result["structured_content"] = data;
            }
            self.send(json!({"jsonrpc":"2.0","id":id,"result":result}));
        }
        self.notice(json!({"kind":"terminal","request":id}));
    }
}
fn main() {
    let state = Arc::new(State::default());
    state.log(json!({"kind":"process"}));
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let params = &message["params"];
        match message["method"].as_str() {
            None => {
                if let Some(tx) = state
                    .responses
                    .lock()
                    .unwrap()
                    .remove(message["id"].as_str().unwrap())
                {
                    let _ = tx.send(message);
                }
            }
            Some("initialize") => {
                let tools: Value =
                    serde_json::from_slice(&std::fs::read("catalog.json").unwrap()).unwrap();
                let mut features = params["protocol"]["required_features"]
                    .as_array()
                    .unwrap()
                    .clone();
                let selected: Vec<Value> = std::fs::read("features.json")
                    .ok()
                    .map(|bytes| serde_json::from_slice(&bytes).unwrap())
                    .unwrap_or_else(|| {
                        vec![json!("resource_refs_v1"), json!("operation_descriptors_v1")]
                    });
                features.extend(selected);
                *state.transport.lock().unwrap() = params["protocol"]["bulk_objects_v1"].clone();
                // Echo only the limits this peer accepts. The physical frame
                // bound and the paired session profile are host offers for a
                // transport negotiation this fixture never selects.
                let mut limits = params["protocol"]["limits"].clone();
                limits.as_object_mut().unwrap().remove("max_message_bytes");
                state.send(json!({"jsonrpc":"2.0","id":message["id"],"result":{"api_version":"0.4","tools":tools,"protocol":{"version":"0.4","features":features,"limits":limits}}}));
            }
            Some("tool/call") => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || state.call(message));
            }
            Some("fixture/malformed") => {
                state.send(json!({"jsonrpc":"2.0","id":params["request"]}))
            }
            Some("fixture/allow") => {
                let barrier =
                    state.barriers.lock().unwrap()[&params["request"].as_u64().unwrap()].clone();
                *barrier.0.lock().unwrap() = true;
                barrier.1.notify_all();
            }
            Some("$/cancelRequest") => {
                state.notice(json!({"kind":"cancelled","request":params["id"]}))
            }
            Some("resource/dispose") => {
                let mode =
                    std::fs::read_to_string("cleanup-mode").unwrap_or_else(|_| "completed".into());
                let refs = params["resources"].as_array().unwrap();
                state.log(json!({"kind":"dispose","resources":refs}));
                for reference in refs {
                    state
                        .objects
                        .lock()
                        .unwrap()
                        .remove(reference["$resource"].as_str().unwrap());
                }
                if mode != "hang" {
                    state.send(json!({"jsonrpc":"2.0","id":message["id"],"result":{"results":refs.iter().map(|r| json!({"resource":r,"status":mode})).collect::<Vec<_>>()}}));
                    state.notice(json!({"kind":"disposed","resources":refs}));
                }
            }
            Some("shutdown") => {
                state.send(json!({"jsonrpc":"2.0","id":message["id"],"result":{}}));
                return;
            }
            _ => {}
        }
    }
}
