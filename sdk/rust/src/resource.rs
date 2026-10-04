//! Nominal references and process-local native values. The host owns admission.
use crate::{reverse::Reverse, CallContext, Error};
use schemars::{gen::SchemaGenerator, schema::Schema, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

pub(crate) const FEATURES: [&str; 2] = ["resource_refs_v1", "operation_descriptors_v1"];
pub(crate) fn limits() -> Value {
    json!({"max_records":256,"max_registrations_per_parent":32})
}

/// Declare native identity once; native state is never serialized.
/// Cleanup runs on the serialized execution lane, not the protocol reader.
pub trait ResourceType: Send + Sized + 'static {
    const TYPE_ID: &'static str;
    fn dispose(self) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    #[serde(rename = "$resource")]
    token: String,
    #[serde(rename = "type")]
    nominal: String,
}
impl Reference {
    fn valid(&self) -> bool {
        !self.token.is_empty()
            && self.token.len() <= 128
            && self.token.bytes().all(|b| b.is_ascii_graphic())
            && nominal(&self.nominal)
    }
}
/// A typed, opaque identity, not ownership of or direct access to a native object.
/// Cloning it never clones native state. Deserialization grants no authority.
pub struct Resource<T: ResourceType> {
    reference: Reference,
    marker: PhantomData<fn() -> T>,
}
impl<T: ResourceType> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            marker: PhantomData,
        }
    }
}
impl<T: ResourceType> std::fmt::Debug for Resource<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.reference.fmt(f)
    }
}
impl<T: ResourceType> Serialize for Resource<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.reference.serialize(serializer)
    }
}
impl<'de, T: ResourceType> Deserialize<'de> for Resource<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let reference = Reference::deserialize(deserializer)?;
        if !reference.valid() || reference.nominal != T::TYPE_ID {
            return Err(serde::de::Error::custom(
                "invalid nominal resource reference",
            ));
        }
        Ok(Self {
            reference,
            marker: PhantomData,
        })
    }
}
impl<T: ResourceType> JsonSchema for Resource<T> {
    fn schema_name() -> String {
        format!("Resource_{}", T::TYPE_ID)
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        serde_json::from_value(json!({"type":"object","additionalProperties":false,
            "properties":{"$resource":{"type":"string"},"type":{"type":"string","enum":[T::TYPE_ID]}},
            "required":["$resource","type"]})).unwrap()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    Pending,
    Completed,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseStatus {
    pub retired: bool,
    pub cleanup: CleanupStatus,
}

fn nominal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic() || (i > 0 && (b.is_ascii_digit() || b"_.-".contains(&b)))
        })
}
fn unavailable() -> Error {
    Error::invalid("resource_unavailable")
}

/// Discover every slot from generated types, refusing untrackable shapes.
pub(crate) fn operation(
    name: &str,
    input: &Value,
    output: &Value,
    receiver: Option<&str>,
    explicit: bool,
) -> Result<Option<Value>, Error> {
    fn slots(
        schema: &Value,
        path: &str,
        fixed: bool,
        input: bool,
        found: &mut Vec<Value>,
    ) -> Result<(), Error> {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            if props.contains_key("$resource") {
                let ty = props
                    .get("type")
                    .and_then(|s| s.get("enum"))
                    .and_then(Value::as_array);
                let ty = ty
                    .filter(|v| v.len() == 1)
                    .and_then(|v| v[0].as_str())
                    .filter(|s| nominal(s))
                    .ok_or_else(|| Error::invalid("invalid nominal Resource schema"))?;
                if !fixed
                    || path.is_empty()
                    || path.len() > 1024
                    || schema["type"] != "object"
                    || schema["additionalProperties"] != false
                    || props.len() != 2
                    || props["$resource"]["type"] != "string"
                    || props["type"]["type"] != "string"
                    || schema["required"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect::<BTreeSet<_>>())
                        != Some(BTreeSet::from(["$resource", "type"]))
                    || ["anyOf", "allOf", "oneOf", "$ref"]
                        .iter()
                        .any(|k| schema.get(*k).is_some())
                {
                    return Err(Error::invalid(
                        "resources require fixed object-property slots",
                    ));
                }
                let mut slot = json!({"path":path,"type":ty});
                if input {
                    slot["access"] = "exclusive".into();
                }
                found.push(slot);
                return Ok(());
            }
            let fixed = fixed
                && schema["type"] == "object"
                && !["anyOf", "allOf", "oneOf", "$ref"]
                    .iter()
                    .any(|k| schema.get(*k).is_some());
            for (key, child) in props {
                slots(
                    child,
                    &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                    fixed,
                    input,
                    found,
                )?;
            }
        }
        for key in ["items", "additionalProperties", "anyOf", "allOf", "oneOf"] {
            if let Some(value) = schema.get(key) {
                if let Some(array) = value.as_array() {
                    for child in array {
                        slots(child, path, false, input, found)?;
                    }
                } else {
                    slots(value, path, false, input, found)?;
                }
            }
        }
        Ok(())
    }
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    slots(input, "", true, true, &mut inputs)?;
    slots(output, "", true, false, &mut outputs)?;
    if !explicit && inputs.is_empty() && outputs.is_empty() {
        return Ok(None);
    }
    if !nominal(name) || receiver.is_some_and(|p| !inputs.iter().any(|v| v["path"] == p)) {
        return Err(Error::invalid("invalid operation id or receiver slot"));
    }
    let mut descriptor = json!({"id":name,"resource_inputs":inputs,"resource_outputs":outputs});
    if let Some(receiver) = receiver {
        descriptor["receiver"] = receiver.into();
    }
    Ok(Some(descriptor))
}

struct Native {
    value: Box<dyn Any + Send>,
    dispose: fn(Box<dyn Any + Send>) -> Result<(), Error>,
}
impl Native {
    fn new<T: ResourceType>(value: T) -> Self {
        Self {
            value: Box::new(value),
            dispose: |value| (*value.downcast::<T>().unwrap()).dispose(),
        }
    }
    fn dispose(self) -> bool {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.dispose)(self.value)))
            .is_ok_and(|r| r.is_ok())
    }
}
struct Entry {
    reference: Reference,
    owner: Value,
    retired: bool,
    native: Arc<Mutex<Option<Native>>>,
}
pub(crate) struct Runtime {
    entries: Mutex<BTreeMap<String, Entry>>,
    pub reverse: Reverse,
}
pub(crate) struct Call {
    runtime: Arc<Runtime>,
    parent: u64,
    owner: Value,
    inputs: BTreeSet<String>,
    created: Mutex<Vec<Reference>>,
    lane: OnceLock<ThreadId>,
}
impl Runtime {
    pub fn new(writer: crate::protocol::Writer) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            reverse: Reverse::new(writer),
        }
    }
    pub fn prepare(
        self: &Arc<Self>,
        id: &Value,
        context: &Value,
        operation: &Value,
        arguments: &Value,
    ) -> Result<Arc<Call>, Error> {
        let parent = id.as_u64().ok_or_else(unavailable)?;
        let owner = context["resource_owner"].clone();
        if !owner.is_object()
            || !owner["session_id"].is_string()
            || !owner["extension_instance_id"].is_string()
            || !owner["process_generation"].is_u64()
        {
            return Err(unavailable());
        }
        let mut inputs = BTreeSet::new();
        for slot in operation["resource_inputs"].as_array().unwrap() {
            if let Some(value) = arguments.pointer(slot["path"].as_str().unwrap()) {
                let reference: Reference =
                    serde_json::from_value(value.clone()).map_err(|_| unavailable())?;
                self.lookup(&owner, &reference)?;
                inputs.insert(reference.token);
            }
        }
        Ok(Arc::new(Call {
            runtime: self.clone(),
            parent,
            owner,
            inputs,
            created: Mutex::new(Vec::new()),
            lane: OnceLock::new(),
        }))
    }
    fn lookup(
        &self,
        owner: &Value,
        reference: &Reference,
    ) -> Result<Arc<Mutex<Option<Native>>>, Error> {
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(&reference.token)
            .filter(|e| !e.retired && &e.owner == owner && &e.reference == reference)
            .ok_or_else(unavailable)?;
        Ok(entry.native.clone())
    }
    pub fn retire(&self, references: &[Reference]) {
        let mut entries = self.entries.lock().unwrap();
        for reference in references {
            if let Some(entry) = entries
                .get_mut(&reference.token)
                .filter(|e| &e.reference == reference)
            {
                entry.retired = true;
            }
        }
    }
    pub fn references(&self) -> Vec<Reference> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| e.reference.clone())
            .collect()
    }
    pub fn dispose(&self, references: Vec<Reference>) -> Value {
        // Invalidate/remove the whole batch before any user destructor runs.
        let removed = {
            let mut entries = self.entries.lock().unwrap();
            references
                .into_iter()
                .map(|reference| {
                    let entry = if entries
                        .get(&reference.token)
                        .is_some_and(|e| e.reference == reference)
                    {
                        entries.remove(&reference.token)
                    } else {
                        None
                    };
                    (reference, entry)
                })
                .collect::<Vec<_>>()
        };
        let results = removed
            .into_iter()
            .map(|(reference, entry)| {
                let completed = entry
                    .and_then(|e| e.native.lock().unwrap_or_else(|p| p.into_inner()).take())
                    .is_some_and(Native::dispose);
                json!({"resource":reference,"status":if completed {"completed"} else {"failed"}})
            })
            .collect::<Vec<_>>();
        json!({"results":results})
    }
}
impl Call {
    pub fn enter(&self) {
        let _ = self.lane.set(std::thread::current().id());
    }
    fn check(&self, context: &CallContext) -> Result<(), Error> {
        context.check_cancelled()?;
        if self.lane.get() != Some(&std::thread::current().id())
            || context.terminal.lock().unwrap().settled
        {
            return Err(Error::invalid(
                "native resources require the active handler execution lane",
            ));
        }
        Ok(())
    }
    pub fn validate_output(&self, operation: &Value, result: &Value) -> Result<(), Error> {
        for slot in operation["resource_outputs"].as_array().unwrap() {
            if let Some(value) =
                result["structured_content"].pointer(slot["path"].as_str().unwrap())
            {
                let reference =
                    serde_json::from_value(value.clone()).map_err(|_| Error::internal())?;
                self.runtime
                    .lookup(&self.owner, &reference)
                    .map_err(|_| Error::internal())?;
            }
        }
        Ok(())
    }
    pub fn settle(&self, result: Option<&Value>, operation: &Value) {
        let outputs = operation["resource_outputs"].as_array().unwrap();
        let retired = self
            .created
            .lock()
            .unwrap()
            .iter()
            .filter(|reference| {
                !result.is_some_and(|r| {
                    r["is_error"] == false
                        && outputs.iter().any(|s| {
                            r["structured_content"]
                                .pointer(s["path"].as_str().unwrap())
                                .is_some_and(|v| v["$resource"] == reference.token)
                        })
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        self.runtime.retire(&retired);
    }
}

impl CallContext {
    fn resources(&self) -> Result<&Arc<Call>, Error> {
        let call = self.resources.as_ref().ok_or_else(|| {
            Error::rpc(-32601, "resources were not negotiated for this operation")
        })?;
        call.check(self)?;
        Ok(call)
    }
    /// Export an owned native value provisionally. Only host result admission makes it live.
    /// A failed registration disposes the unpublished value on this handler's lane.
    pub fn export<T: ResourceType>(&self, value: T) -> Result<Resource<T>, Error> {
        let mut native = Some(Native::new(value));
        let result = (|| {
            let call = self.resources()?;
            if !nominal(T::TYPE_ID) {
                return Err(Error::invalid("invalid resource type id"));
            }
            if call.created.lock().unwrap().len() >= 32
                || call.runtime.entries.lock().unwrap().len() >= 256
            {
                return Err(Error::rpc(-32000, "resource quota exceeded"));
            }
            let response = call.runtime.reverse.request(
                self,
                call.parent,
                "resource/register",
                json!({"type":T::TYPE_ID}),
            )?;
            let reference: Reference =
                serde_json::from_value(response).map_err(|_| Error::internal())?;
            if !reference.valid() || reference.nominal != T::TYPE_ID {
                return Err(Error::internal());
            }
            let mut entries = call.runtime.entries.lock().unwrap();
            if entries.contains_key(&reference.token) {
                return Err(Error::internal());
            }
            entries.insert(
                reference.token.clone(),
                Entry {
                    reference: reference.clone(),
                    owner: call.owner.clone(),
                    retired: false,
                    native: Arc::new(Mutex::new(native.take())),
                },
            );
            call.created.lock().unwrap().push(reference.clone());
            Ok(Resource {
                reference,
                marker: PhantomData,
            })
        })();
        if let Some(native) = native {
            if !native.dispose() {
                eprintln!("octet-native: unpublished resource cleanup failed");
            }
        }
        result
    }
    /// Borrow only this call's declared input or newly exported native value, exclusively.
    pub fn with_resource<T: ResourceType, R>(
        &self,
        resource: &Resource<T>,
        handler: impl FnOnce(&mut T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let call = self.resources()?;
        if !call.inputs.contains(&resource.reference.token)
            && !call.created.lock().unwrap().contains(&resource.reference)
        {
            return Err(unavailable());
        }
        let native = call.runtime.lookup(&call.owner, &resource.reference)?;
        let mut native = native
            .try_lock()
            .map_err(|_| Error::invalid("resource_busy"))?;
        let value = native
            .as_mut()
            .and_then(|n| n.value.downcast_mut::<T>())
            .ok_or_else(unavailable)?;
        handler(value)
    }
    /// Retire an unpinned host reference. Cleanup is separate; pinned inputs return busy.
    pub fn release<T: ResourceType>(&self, resource: &Resource<T>) -> Result<ReleaseStatus, Error> {
        let call = self.resources()?;
        call.runtime.lookup(&call.owner, &resource.reference)?;
        let response = call.runtime.reverse.request(
            self,
            call.parent,
            "resource/release",
            json!({"resource":resource.reference}),
        )?;
        let status: ReleaseStatus =
            serde_json::from_value(response).map_err(|_| Error::internal())?;
        if !status.retired {
            return Err(Error::internal());
        }
        call.runtime
            .retire(std::slice::from_ref(&resource.reference));
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Counter;
    impl ResourceType for Counter {
        const TYPE_ID: &'static str = "demo.Counter";
    }
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Input {
        first: Resource<Counter>,
        nested: Nested,
    }
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Nested {
        second: Resource<Counter>,
    }
    #[test]
    fn nominal_codec_and_generated_slots_have_one_source() {
        let input = crate::schema::typed_generated::<Input>(true).unwrap();
        let output = crate::schema::typed_generated::<Nested>(false).unwrap();
        assert_eq!(
            input["properties"]["first"]["properties"]["type"]["enum"],
            json!([Counter::TYPE_ID])
        );
        let op = operation("sum", &input, &output, Some("/nested/second"), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            op["resource_inputs"],
            json!([
                {"path":"/first","type":"demo.Counter","access":"exclusive"},
                {"path":"/nested/second","type":"demo.Counter","access":"exclusive"}
            ])
        );
        assert_eq!(
            op["resource_outputs"],
            json!([{"path":"/second","type":"demo.Counter"}])
        );
        assert!(operation("sum", &input, &output, Some("/missing"), true).is_err());
        let value = json!({"$resource":"opaque","type":Counter::TYPE_ID});
        let reference: Resource<Counter> = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reference.clone()).unwrap(), value);
        assert!(serde_json::from_value::<Resource<Counter>>(
            json!({"$resource":"opaque","type":"other.Type"})
        )
        .is_err());
        assert!(serde_json::from_value::<Resource<Counter>>(
            json!({"$resource":"opaque","type":Counter::TYPE_ID,"extra":1})
        )
        .is_err());
    }
    #[test]
    fn untrackable_resource_shapes_fail_registration() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Arrays {
            values: Vec<Resource<Counter>>,
        }
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Nullable {
            value: Option<Resource<Counter>>,
        }
        let empty =
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false});
        for shape in [
            crate::schema::typed_generated::<Arrays>(false).unwrap(),
            crate::schema::typed_generated::<Nullable>(false).unwrap(),
            crate::schema::typed_generated::<Resource<Counter>>(false).unwrap(),
        ] {
            assert!(operation("create", &empty, &shape, None, false).is_err());
        }
        assert!(operation("ordinary", &empty, &empty, None, false)
            .unwrap()
            .is_none());
    }
    #[test]
    fn malformed_disposal_is_atomic() {
        let reference = json!({"$resource":"opaque","type":Counter::TYPE_ID});
        assert!(disposal(&json!({"resources":[reference.clone()],"reason":"retired"})).is_ok());
        assert!(
            disposal(&json!({"resources":[reference.clone(),reference],"reason":"retired"}))
                .is_err()
        );
        assert!(disposal(&json!({"resources":[],"reason":"retired"})).is_err());
    }
}

pub(crate) fn disposal(params: &Value) -> Result<Vec<Reference>, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        resources: Vec<Reference>,
        reason: String,
    }
    let request: Request = serde_json::from_value(params.clone())
        .map_err(|_| Error::invalid("invalid disposal request"))?;
    let mut tokens = BTreeSet::new();
    if request.resources.is_empty()
        || request.resources.len() > 32
        || request.reason != "retired"
        || request
            .resources
            .iter()
            .any(|r| !r.valid() || !tokens.insert(&r.token))
    {
        return Err(Error::invalid("invalid disposal request"));
    }
    Ok(request.resources)
}
