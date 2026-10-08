//! Minimal embedding of the *existing* quickjs-wasi 3.6.2 reactor.
//! No WASI filesystem, environment, arguments, sockets, or preopens are linked.
//! The module is compiled and validated once per runner lifetime; every script
//! instantiates it in a fresh, isolated Wasmi store.
use super::runner::emit;
use crate::guest;
use crate::runner::Start;
use crate::IPC_BYTES;
use anyhow::{bail, Context, Result};
use std::time::Instant;
use wasmi::{
    AsContextMut, Caller, Engine as WasmiEngine, Instance, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder, WasmParams, WasmResults,
};

struct State {
    deadline: Instant,
    limits: StoreLimits,
}

fn call<P: WasmParams, R: WasmResults>(
    store: &mut impl AsContextMut<Data = State>,
    instance: Instance,
    name: &str,
    args: P,
) -> Result<R> {
    Ok(instance
        .get_typed_func::<P, R>(&*store, name)?
        .call(store, args)?)
}
fn exported<P: WasmParams, R: WasmResults>(
    caller: &mut Caller<'_, State>,
    name: &str,
    args: P,
) -> Result<R, wasmi::Error> {
    caller
        .get_export(name)
        .and_then(|x| x.into_func())
        .ok_or_else(|| wasmi::Error::new(format!("missing export {name}")))?
        .typed::<P, R>(&*caller)?
        .call(caller, args)
}
fn mem(caller: &Caller<'_, State>) -> Memory {
    caller.get_export("memory").unwrap().into_memory().unwrap()
}
fn read_u32(
    memory: Memory,
    store: impl wasmi::AsContext<Data = State>,
    ptr: i32,
) -> Result<i32, wasmi::Error> {
    let mut bytes = [0; 4];
    memory
        .read(store, ptr as u32 as usize, &mut bytes)
        .map_err(|e| wasmi::Error::new(e.to_string()))?;
    Ok(i32::from_le_bytes(bytes))
}
fn string_arg(caller: &mut Caller<'_, State>, handle: i32) -> Result<String, wasmi::Error> {
    if exported::<_, i32>(caller, "qjs_is_string", handle)? == 0 {
        return Err(wasmi::Error::new("bridge expects string"));
    }
    let len_ptr: i32 = exported(caller, "wasm_malloc", 4i32)?;
    if len_ptr == 0 {
        return Err(wasmi::Error::new("WASM allocation failed"));
    }
    let ptr: i32 = exported(caller, "qjs_get_string_len", (handle, len_ptr))?;
    let memory = mem(caller);
    let len = read_u32(memory, &*caller, len_ptr)? as u32 as usize;
    if ptr == 0 || len > IPC_BYTES {
        return Err(wasmi::Error::new("invalid bridge string size"));
    }
    let mut bytes = vec![0; len];
    memory
        .read(&*caller, ptr as u32 as usize, &mut bytes)
        .map_err(|e| wasmi::Error::new(e.to_string()))?;
    exported::<_, ()>(caller, "qjs_free_cstring", ptr)?;
    exported::<_, ()>(caller, "wasm_free", len_ptr)?;
    String::from_utf8(bytes).map_err(|e| wasmi::Error::new(e.to_string()))
}
// quickjs-wasi expects seconds east of UTC for a signed 64-bit Unix timestamp,
// split into high/low words. Match its default host-timezone callback.
fn host_timezone_offset(hi: i32, lo: i32) -> i32 {
    #[cfg(unix)]
    {
        let seconds = ((hi as i64) << 32) | i64::from(lo as u32);
        let seconds = seconds as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        // localtime_r initializes `local` on success and reports range errors
        // with null. It applies the process TZ, just like native QuickJS.
        unsafe {
            if !libc::localtime_r(&seconds, local.as_mut_ptr()).is_null() {
                return local.assume_init().tm_gmtoff as i32;
            }
        }
    }
    0
}

fn imports(engine: &WasmiEngine) -> Result<Linker<State>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap(
        "env",
        "host_call",
        |mut c: Caller<'_, State>,
         _name: i32,
         _len: i32,
         _this: i32,
         argc: i32,
         argv: i32|
         -> Result<i32, wasmi::Error> {
            if argc != 1 {
                return Err(wasmi::Error::new("invalid emitter argument count"));
            }
            let arg = read_u32(mem(&c), &c, argv)?;
            let text = string_arg(&mut c, arg)?;
            emit(&text).map_err(|e| wasmi::Error::new(e.to_string()))?;
            exported(&mut c, "qjs_get_undefined", ())
        },
    )?;
    linker.func_wrap("env", "host_interrupt", |c: Caller<'_, State>| -> i32 {
        (Instant::now() >= c.data().deadline) as i32
    })?;
    linker.func_wrap("env", "host_get_timezone_offset", host_timezone_offset)?;
    linker.func_wrap("env", "host_module_normalize", |_: i32, _: i32| -> i32 {
        0
    })?;
    linker.func_wrap("env", "host_module_load", |_: i32, _: i32| -> i32 { 0 })?;
    linker.func_wrap(
        "env",
        "host_promise_rejection",
        |mut c: Caller<'_, State>, p: i32, r: i32, _: i32| -> Result<(), wasmi::Error> {
            exported::<_, ()>(&mut c, "qjs_free_value", p)?;
            exported::<_, ()>(&mut c, "qjs_free_value", r)
        },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "clock_time_get",
        |mut c: Caller<'_, State>, clock: i32, _: i64, ptr: i32| -> i32 {
            if clock != 0 && clock != 1 {
                return 52;
            }
            let ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            if mem(&c)
                .write(&mut c, ptr as u32 as usize, &ns.to_le_bytes())
                .is_ok()
            {
                0
            } else {
                21
            }
        },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "random_get",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| -> i32 {
            // libc's OS random source, not inherited descriptors or guest access.
            if len < 0 || len as usize > IPC_BYTES {
                return 28;
            }
            let mut bytes = vec![0; len as usize];
            #[cfg(unix)]
            {
                use std::io::Read;
                if std::fs::File::open("/dev/urandom")
                    .and_then(|mut f| f.read_exact(&mut bytes))
                    .is_err()
                {
                    return 29;
                }
            }
            #[cfg(not(unix))]
            {
                return 52;
            }
            if mem(&c).write(&mut c, ptr as u32 as usize, &bytes).is_ok() {
                0
            } else {
                21
            }
        },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_write",
        |mut c: Caller<'_, State>,
         fd: i32,
         iov: i32,
         count: i32,
         out: i32|
         -> Result<i32, wasmi::Error> {
            if fd != 1 && fd != 2 {
                return Ok(8);
            }
            if !(0..=1024).contains(&count) {
                return Ok(28);
            }
            let memory = mem(&c);
            let mut total = 0u32;
            for index in 0..count {
                total = total
                    .checked_add(read_u32(memory, &c, iov + index * 8 + 4)? as u32)
                    .ok_or_else(|| wasmi::Error::new("fd_write overflow"))?;
            }
            // Engine diagnostics discarded; never mixed into the protocol stream.
            memory
                .write(&mut c, out as u32 as usize, &total.to_le_bytes())
                .map_err(|e| wasmi::Error::new(e.to_string()))?;
            Ok(0)
        },
    )?;
    linker.func_wrap("wasi_snapshot_preview1", "fd_close", |_: i32| -> i32 { 52 })?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_seek",
        |_: i32, _: i64, _: i32, _: i32| -> i32 { 52 },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "fd_fdstat_get",
        |mut c: Caller<'_, State>, fd: i32, ptr: i32| -> i32 {
            if fd != 1 && fd != 2 {
                return 8;
            }
            let mut data = [0u8; 24];
            data[0] = 2;
            if mem(&c).write(&mut c, ptr as u32 as usize, &data).is_ok() {
                0
            } else {
                21
            }
        },
    )?;
    Ok(linker)
}

/// The module is compiled and validated once for the runner lifetime, and the
/// invariant guest program is compiled to QuickJS bytecode once per process;
/// every script gets a fresh store that only re-executes that bytecode.
pub(super) struct Engine {
    engine: WasmiEngine,
    module: Module,
    linker: Linker<State>,
    program: std::sync::Mutex<Option<Vec<u8>>>,
}

impl Engine {
    pub fn new() -> Result<Self> {
        // Let QuickJS's 512 KiB shadow-stack guard fire before Wasmi's own
        // execution-stack ceiling, preserving catchable JS recursion errors.
        let mut config = wasmi::Config::default();
        config.set_stack_limits(wasmi::StackLimits::new(1024, 1024 * 1024, 16_384)?);
        let engine = WasmiEngine::new(&config);
        // The pinned module is embedded unchanged. No compiler or SDK at runtime.
        let module = Module::new(
            &engine,
            include_bytes!("../vendor/quickjs-wasi/quickjs.wasm"),
        )?;
        let linker = imports(&engine)?;
        Ok(Self {
            engine,
            module,
            linker,
            program: std::sync::Mutex::new(None),
        })
    }
    pub fn session(&self, start: &Start, deadline: Instant) -> Result<Session> {
        let mut store = Store::new(
            &self.engine,
            State {
                deadline,
                limits: StoreLimitsBuilder::new()
                    .memory_size(start.heap_bytes + 64 * 1024 * 1024)
                    .instances(1)
                    .memories(1)
                    .build(),
            },
        );
        store.limiter(|s| &mut s.limits);
        let instance = self
            .linker
            .instantiate(&mut store, &self.module)?
            .start(&mut store)?;
        let memory = instance
            .get_memory(&store, "memory")
            .context("missing memory")?;
        let mut session = Session {
            store,
            instance,
            memory,
            driver: 0,
            undefined: 0,
        };
        session.call::<_, ()>("_initialize", ())?;
        session.call::<_, i32>("qjs_init", ())?;
        session.call::<_, ()>("qjs_set_memory_limit", start.heap_bytes as i32)?;
        session.call::<_, ()>("qjs_set_max_stack_size", 512 * 1024i32)?;
        session.call::<_, ()>("qjs_set_interrupt_handler", 1i32)?;
        let global: i32 = session.call("qjs_get_global", ())?;
        let name = session.bytes(b"__emit")?;
        let function: i32 = session.call("qjs_new_host_function", (name, 6i32, 0i32))?;
        session.call::<_, i32>("qjs_set_prop_string", (global, name, function))?;
        session.call::<_, ()>("wasm_free", name)?;
        session.free(function)?;
        session.free(global)?;
        // The invariant guest program is parsed once; each fresh store only
        // re-executes the cached bytecode and binds the script's own arguments.
        let cached = self.program.lock().unwrap().clone();
        let (bytecode, compiled) = match cached {
            Some(bytecode) => (bytecode, false),
            None => (session.compile(guest::prelude())?, true),
        };
        let factory = session.bytecode(&bytecode)?;
        if compiled {
            *self.program.lock().unwrap() = Some(bytecode);
        }
        let context = session.string(&start.context)?;
        let code = session.string(&start.code)?;
        let mut argv = Vec::with_capacity(8);
        argv.extend_from_slice(&context.to_le_bytes());
        argv.extend_from_slice(&code.to_le_bytes());
        let argv = session.bytes(&argv)?;
        let result = session.call("qjs_call", (factory, session.undefined(), 2i32, argv))?;
        session.check(result)?;
        session.call::<_, ()>("wasm_free", argv)?;
        session.free(context)?;
        session.free(code)?;
        session.free(factory)?;
        session.driver = result;
        session.undefined = session.call("qjs_get_undefined", ())?;
        Ok(session)
    }
}

/// One script's isolated QuickJS instance and linear memory. Dropping it
/// releases the whole VM; nothing is pooled or reused.
pub(super) struct Session {
    store: Store<State>,
    instance: Instance,
    memory: Memory,
    driver: i32,
    undefined: i32,
}

impl Session {
    fn call<P: WasmParams, R: WasmResults>(&mut self, name: &str, args: P) -> Result<R> {
        call(&mut self.store, self.instance, name, args)
    }
    fn undefined(&self) -> i32 {
        self.undefined
    }
    /// Compile source to QuickJS bytecode without executing it.
    fn compile(&mut self, source: &str) -> Result<Vec<u8>> {
        let code = self.bytes(source.as_bytes())?;
        let file = self.bytes(b"codemode-prelude.js")?;
        let out_len = self.call::<_, i32>("wasm_malloc", 4i32)?;
        let buffer = self.call::<_, i32>(
            "qjs_compile",
            (code, source.len() as i32, file, 0i32, 0i32, out_len),
        )?;
        if buffer == 0 {
            let exception: i32 = self.call("qjs_get_exception", ())?;
            let length: i32 = self.call("wasm_malloc", 4i32)?;
            let ptr: i32 = self.call("qjs_get_string_len", (exception, length))?;
            let size = read_u32(self.memory, &self.store, length)? as u32 as usize;
            let mut bytes = vec![0; size.min(4096)];
            self.memory
                .read(&self.store, ptr as u32 as usize, &mut bytes)?;
            bail!("QuickJS/WASI compile: {}", String::from_utf8_lossy(&bytes));
        }
        let size = read_u32(self.memory, &self.store, out_len)? as u32 as usize;
        let mut bytes = vec![0; size];
        self.memory
            .read(&self.store, buffer as u32 as usize, &mut bytes)?;
        self.call::<_, ()>("wasm_free", buffer)?;
        self.call::<_, ()>("wasm_free", out_len)?;
        self.call::<_, ()>("wasm_free", code)?;
        self.call::<_, ()>("wasm_free", file)?;
        Ok(bytes)
    }
    /// Execute cached bytecode and keep its value handle.
    fn bytecode(&mut self, bytecode: &[u8]) -> Result<i32> {
        let ptr = self.bytes(bytecode)?;
        let result = self.call("qjs_eval_bytecode", (ptr, bytecode.len() as i32))?;
        self.call::<_, ()>("wasm_free", ptr)?;
        self.check(result)
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<i32> {
        let ptr: i32 = self.call("wasm_malloc", (bytes.len() + 1) as i32)?;
        if ptr == 0 {
            bail!("WASM allocation failed");
        }
        self.memory
            .write(&mut self.store, ptr as u32 as usize, bytes)?;
        self.memory
            .write(&mut self.store, ptr as u32 as usize + bytes.len(), &[0])?;
        Ok(ptr)
    }
    fn string(&mut self, text: &str) -> Result<i32> {
        let ptr = self.bytes(text.as_bytes())?;
        let value = self.call("qjs_new_string", (ptr, text.len() as i32))?;
        self.call::<_, ()>("wasm_free", ptr)?;
        Ok(value)
    }
    fn check(&mut self, value: i32) -> Result<i32> {
        if self.call::<_, i32>("qjs_is_exception", value)? != 0 {
            let err: i32 = self.call("qjs_get_exception", ())?;
            let length: i32 = self.call("wasm_malloc", 4i32)?;
            let ptr: i32 = self.call("qjs_get_string_len", (err, length))?;
            let size = read_u32(self.memory, &self.store, length)? as u32 as usize;
            let mut bytes = vec![0; size.min(4096)];
            self.memory
                .read(&self.store, ptr as u32 as usize, &mut bytes)?;
            bail!("QuickJS/WASI: {}", String::from_utf8_lossy(&bytes));
        }
        Ok(value)
    }
    fn free(&mut self, value: i32) -> Result<()> {
        self.call("qjs_free_value", value)
    }
    fn drive(&mut self, payload: &str) -> Result<()> {
        let string = self.string(payload)?;
        let argv = self.bytes(&string.to_le_bytes())?;
        let result = self.call("qjs_call", (self.driver, self.undefined, 1i32, argv))?;
        self.check(result)?;
        self.free(result)?;
        self.free(string)?;
        self.call::<_, ()>("wasm_free", argv)?;
        while self.call::<_, i32>("qjs_is_job_pending", ())? != 0 {
            if self.call::<_, i32>("qjs_execute_pending_job", ())? < 0 {
                bail!("QuickJS/WASI pending job failed");
            }
        }
        Ok(())
    }
    /// Feed one guest command through the driver function.
    pub fn message(&mut self, payload: &str) -> Result<()> {
        self.drive(payload)
    }
}
