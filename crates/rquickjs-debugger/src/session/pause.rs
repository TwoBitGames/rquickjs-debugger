use std::cell::RefCell;
use std::ffi::{c_int, c_void};
use std::ptr::NonNull;

use rquickjs::{Ctx, Object, Value, qjs};
use serde_json::{Value as Json, json};
use tracing::warn;

use super::{Flow, Session};
use crate::engine::{self, Frame, PauseReason, ScopeKind, StepMode};
use crate::protocol::{CallFrameId, CdpError};

impl Session {
    fn serve_pause(&mut self, ctx: &Ctx<'_>, reason: PauseReason, exception: qjs::JSValueConst) {
        let Some(runtime) = self.runtime() else {
            return;
        };
        if self.client.is_none() || !self.debugger_enabled || self.skip_all_pauses {
            return;
        }
        self.flush_scripts(Some(ctx));
        let frames = engine::backtrace(ctx);

        let mut hit_breakpoints = Vec::new();
        if reason == PauseReason::Breakpoint {
            let Some(top) = frames.first() else {
                return;
            };
            hit_breakpoints = self.breakpoints_hit(ctx, &top.file, top.position.line);
            if hit_breakpoints.is_empty() {
                return;
            }
        }

        self.paused = true;
        self.pause_seq += 1;
        let mut params = json!({
            "callFrames": self.call_frames(ctx, &frames),
            "reason": reason.protocol_name(),
            "hitBreakpoints": hit_breakpoints.iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        if reason == PauseReason::Exception {
            let value = unsafe {
                Value::from_raw(
                    ctx.clone(),
                    qjs::JS_DupValue(ctx.as_raw().as_ptr(), exception),
                )
            };
            params["data"] = self.remote(ctx, value, false, "");
        }
        self.event("Debugger.paused", params);

        let mode = self.wait_for_resume(ctx);

        self.paused = false;
        self.event("Debugger.resumed", json!({}));
        runtime.step(mode);
        self.extend_deadline();
    }

    fn wait_for_resume(&mut self, ctx: &Ctx<'_>) -> StepMode {
        loop {
            match self.inbox.recv() {
                Ok(message) => {
                    if let Flow::Resume(mode) = self.inbound(Some(ctx), message) {
                        return mode;
                    }
                }
                Err(_) => return StepMode::Continue,
            }
        }
    }

    fn call_frames(&mut self, ctx: &Ctx<'_>, frames: &[Frame]) -> Vec<Json> {
        let mut out = Vec::with_capacity(frames.len());
        for (index, frame) in frames.iter().enumerate() {
            let Some((script_id, url)) = self
                .script_by_name(&frame.file)
                .map(|s| (s.id, s.url.clone()))
            else {
                continue;
            };
            let index = index as u32;
            let mut this = json!({ "type": "undefined" });
            let mut scope_chain = Vec::with_capacity(3);
            if let Some(local) = engine::frame_scope(ctx, index, ScopeKind::Local) {
                if let Some(value) = take_this(&local) {
                    this = self.remote(ctx, value, false, "");
                }
                scope_chain.push(json!({ "type": "local", "object": self.remote(ctx, local.into_value(), false, "") }));
            }
            if let Some(closure) = engine::frame_scope(ctx, index, ScopeKind::Closure) {
                if !closure.is_empty() {
                    scope_chain
                        .push(json!({ "type": "closure", "object": self.remote(ctx, closure.into_value(), false, "") }));
                }
            }
            let globals = ctx.globals().into_value();
            scope_chain
                .push(json!({ "type": "global", "object": self.remote(ctx, globals, false, "") }));
            out.push(json!({
                "callFrameId": CallFrameId { pause: self.pause_seq, frame: index }.to_string(),
                "functionName": frame.function,
                "location": frame.position.location(script_id),
                "url": url,
                "scopeChain": scope_chain,
                "this": this,
                "canBeRestarted": false,
            }));
        }
        out
    }

    pub(super) fn frame_index(&self, call_frame_id: &str) -> Result<u32, CdpError> {
        self.need_pause()?;
        let id: CallFrameId = call_frame_id.parse()?;
        if id.pause != self.pause_seq {
            return Err(CdpError::invalid_params(
                "that call frame is from an earlier pause",
            ));
        }
        Ok(id.frame)
    }

    pub(super) fn evaluate_in_frame<'js>(
        &self,
        ctx: &Ctx<'js>,
        frame: u32,
        expression: &str,
    ) -> rquickjs::Result<Value<'js>> {
        self.without_pausing(|| engine::evaluate_in_frame(ctx, frame, expression))
    }
}

pub(super) fn frame_this<'js>(ctx: &Ctx<'js>, frame: u32) -> Value<'js> {
    engine::frame_scope(ctx, frame, ScopeKind::Local)
        .and_then(|local| local.get::<_, Value<'js>>("this").ok())
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()))
}

fn take_this<'js>(local: &Object<'js>) -> Option<Value<'js>> {
    let value: Value<'js> = local.get("this").ok()?;
    if value.is_undefined() {
        return None;
    }
    let _ = local.remove("this");
    Some(value)
}

pub(super) unsafe extern "C" fn on_pause(
    ctx: *mut qjs::JSContext,
    reason: c_int,
    exception: qjs::JSValueConst,
    opaque: *mut c_void,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let cell = unsafe { &*opaque.cast::<RefCell<Session>>() };
        let Ok(mut session) = cell.try_borrow_mut() else {
            return;
        };
        let (Some(ctx), Some(reason)) = (NonNull::new(ctx), PauseReason::from_raw(reason)) else {
            return;
        };
        let ctx = unsafe { Ctx::from_raw(ctx) };
        session.serve_pause(&ctx, reason, exception);
    }));
    if result.is_err() {
        warn!("debugger: the pause handler panicked; execution goes on");
    }
}
