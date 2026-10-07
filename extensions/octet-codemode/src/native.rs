//! Optional native QuickJS-NG lane (rquickjs 0.14). The runner process stays
//! warm, but every script gets a brand-new runtime/context, so no binding,
//! global or pending job can survive into the next script.
use super::runner::emit;
use crate::runner::Start;
use anyhow::Result;
use rquickjs::{Context as JsContext, Function, Persistent, Runtime};
use std::time::Instant;

/// The native engine has no module to compile; it exists to keep the runner's
/// engine selection explicit and to mirror the Wasmi lane's shape.
pub(super) struct Engine;

impl Engine {
    pub fn new() -> Self {
        Self
    }
    pub fn session(&self, start: &Start, deadline: Instant) -> Session {
        Session::new(start, deadline)
    }
}

/// Field order is drop order: the bound driver and the context must be
/// released before the runtime, or QuickJS aborts on live objects (only the
/// runtime may be dropped last, never first).
pub(super) struct Session {
    driver: Persistent<Function<'static>>,
    context: JsContext,
    runtime: Runtime,
}

impl Session {
    fn new(start: &Start, deadline: Instant) -> Self {
        // Deliberately infallible at construction: a runtime/context failure is
        // reported by the first message, where the runner can classify it.
        let runtime = Runtime::new().expect("QuickJS runtime");
        runtime.set_memory_limit(start.heap_bytes);
        runtime.set_max_stack_size(512 * 1024);
        runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let context = JsContext::full(&runtime).expect("QuickJS context");
        // Full ECMAScript intrinsics, but no std/os modules, loader, filesystem,
        // timers, network, or process functions. Only the private emitter is added.
        let driver = context.with(|ctx| {
            let output = Function::new(ctx.clone(), |text: String| {
                if emit(&text).is_err() {
                    std::process::exit(126);
                }
            })?;
            ctx.globals().set("__emit", output)?;
            // The invariant program is a factory; only the script's own JSON
            // arguments are bound here.
            let factory: Function = ctx
                .eval(crate::guest::prelude())
                .map_err(|e| anyhow::anyhow!("guest: {e}; {:?}", ctx.catch()))?;
            let driver: Function = factory
                .call((start.context.clone(), start.code.clone()))
                .map_err(|e| anyhow::anyhow!("guest: {e}; {:?}", ctx.catch()))?;
            Ok::<_, anyhow::Error>(Persistent::save(&ctx, driver))
        });
        let driver = match driver {
            Ok(driver) => driver,
            Err(error) => {
                // Report through the normal crash path; the runner exits.
                let message: String = format!("{error:#}").chars().take(2048).collect();
                let _ = super::runner::emit_json(
                    &serde_json::json!({"type": "crash", "message": message}),
                );
                std::process::exit(1);
            }
        };
        Session {
            driver,
            context,
            runtime,
        }
    }
    pub fn message(&mut self, payload: &str) -> Result<()> {
        self.context.with(|ctx| -> Result<()> {
            self.driver
                .clone()
                .restore(&ctx)?
                .call::<_, ()>((payload,))
                .map_err(|e| anyhow::anyhow!("call: {e}; {:?}", ctx.catch()))?;
            Ok(())
        })?;
        // Runtime API propagates job exceptions (Ctx API discards them).
        while self
            .runtime
            .execute_pending_job()
            .map_err(|e| anyhow::anyhow!("pending job: {e:?}"))?
        {}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_call_binds_two_strings_and_returns_a_driver() {
        let runtime = Runtime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();
        context.with(|ctx| {
            let factory: Function = ctx
                .eval("(a, b) => { const joined = a + '|' + b; return (command) => joined + ':' + command; }")
                .unwrap();
            let driver: Function = factory.call((String::from("A"), String::from("B"))).unwrap();
            let out: String = driver.call((String::from("C"),)).unwrap();
            assert_eq!(out, "A|B:C");
        });
    }
}
