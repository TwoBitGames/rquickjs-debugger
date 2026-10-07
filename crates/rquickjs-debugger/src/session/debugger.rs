use rquickjs::Ctx;
use serde_json::{Value as Json, json};

use super::breakpoints::{Breakpoint, Where};
use super::{Flow, Session};
use crate::engine::{self, SetVariable, StepMode};
use crate::protocol::{
    CdpError, CdpResult, ContinueToLocationParams, EvaluateOnCallFrameParams,
    GetPossibleBreakpointsParams, GetScriptSourceParams, Location, PauseOnExceptions, Position,
    RemoveBreakpointParams, SetBreakpointByUrlParams, SetBreakpointParams,
    SetBreakpointsActiveParams, SetPauseOnExceptionsParams, SetSkipAllPausesParams,
    SetVariableValueParams, condition, params,
};

impl Session {
    pub(super) fn debugger_domain(
        &mut self,
        ctx: Option<&Ctx<'_>>,
        method: &str,
        p: &Json,
        flow: &mut Flow,
    ) -> CdpResult {
        match method {
            "Debugger.enable" => Ok(self.enable_debugger()),
            "Debugger.disable" => {
                self.debugger_enabled = false;
                self.clear_breakpoints(ctx);
                if self.paused {
                    *flow = Flow::Resume(StepMode::Continue);
                }
                Ok(json!({}))
            }
            "Debugger.setBreakpointsActive" => {
                let p: SetBreakpointsActiveParams = params(p)?;
                self.set_breakpoints_active(p.active);
                Ok(json!({}))
            }
            "Debugger.setSkipAllPauses" => {
                let p: SetSkipAllPausesParams = params(p)?;
                self.skip_all_pauses = p.skip;
                Ok(json!({}))
            }
            "Debugger.setPauseOnExceptions" => {
                let p: SetPauseOnExceptionsParams = params(p)?;
                self.set_pause_on_exceptions(p.state != PauseOnExceptions::None);
                Ok(json!({}))
            }
            "Debugger.setBreakpointByUrl" => self.set_breakpoint_by_url(ctx, params(p)?),
            "Debugger.setBreakpoint" => self.set_breakpoint(ctx, params(p)?),
            "Debugger.removeBreakpoint" => {
                let p: RemoveBreakpointParams = params(p)?;
                self.remove_breakpoint(ctx, p.breakpoint_id.parse()?);
                Ok(json!({}))
            }
            "Debugger.getPossibleBreakpoints" => {
                self.possible_breakpoints(Self::need_ctx(ctx)?, params(p)?)
            }
            "Debugger.getScriptSource" => {
                let p: GetScriptSourceParams = params(p)?;
                let script = self.script(p.script_id.parse()?)?;
                Ok(json!({ "scriptSource": script.source.as_ref() }))
            }
            "Debugger.pause" => {
                match self.runtime() {
                    Some(runtime) if !self.paused => runtime.request_pause(),
                    Some(_) => {}
                    None => self.pause_pending = true,
                }
                Ok(json!({}))
            }
            "Debugger.resume" => {
                if self.paused {
                    *flow = Flow::Resume(StepMode::Continue);
                }
                Ok(json!({}))
            }
            "Debugger.stepInto" => self.step(StepMode::Into, flow),
            "Debugger.stepOver" => self.step(StepMode::Over, flow),
            "Debugger.stepOut" => self.step(StepMode::Out, flow),
            "Debugger.continueToLocation" => {
                self.need_pause()?;
                let p: ContinueToLocationParams = params(p)?;
                self.continue_to(ctx, &p.location)?;
                *flow = Flow::Resume(StepMode::Continue);
                Ok(json!({}))
            }
            "Debugger.evaluateOnCallFrame" => {
                self.evaluate_on_call_frame(Self::need_ctx(ctx)?, params(p)?)
            }
            "Debugger.setVariableValue" => {
                self.set_variable_value(Self::need_ctx(ctx)?, params(p)?)
            }
            "Debugger.searchInContent" => Ok(json!({ "result": [] })),
            "Debugger.setAsyncCallStackDepth"
            | "Debugger.setBlackboxPatterns"
            | "Debugger.setBlackboxedRanges"
            | "Debugger.setBlackboxExecutionContexts"
            | "Debugger.setInstrumentationBreakpoint"
            | "Debugger.removeInstrumentationBreakpoint"
            | "Debugger.setReturnValue" => Ok(json!({})),
            "Debugger.setScriptSource" | "Debugger.restartFrame" | "Debugger.getStackTrace" => {
                Err(CdpError::unsupported())
            }
            _ => Err(CdpError::method_not_found(method)),
        }
    }

    fn enable_debugger(&mut self) -> Json {
        self.debugger_enabled = true;
        let context_id = self.context_id;
        let announcements: Vec<Json> = self
            .scripts
            .iter_mut()
            .map(|script| {
                script.announced = true;
                script.parsed_event(context_id)
            })
            .collect();
        for params in announcements {
            self.defer_event("Debugger.scriptParsed", params);
        }
        json!({ "debuggerId": self.name })
    }

    fn step(&mut self, mode: StepMode, flow: &mut Flow) -> CdpResult {
        self.need_pause()?;
        *flow = Flow::Resume(mode);
        Ok(json!({}))
    }

    fn set_breakpoint_by_url(
        &mut self,
        ctx: Option<&Ctx<'_>>,
        p: SetBreakpointByUrlParams,
    ) -> CdpResult {
        let position = Position::from_protocol(p.line_number, p.column_number);
        let location = match (p.url, p.url_regex) {
            (Some(url), _) => Where::Url(url),
            (None, Some(regex)) => {
                if let Some(ctx) = ctx {
                    self.check_regex(ctx, &regex)?;
                }
                Where::UrlRegex(regex)
            }
            (None, None) => {
                return Err(CdpError::invalid_params("either url or urlRegex is needed"));
            }
        };
        let breakpoint = Breakpoint::new(
            self.fresh_breakpoint_id(),
            location,
            position,
            condition(p.condition),
        );
        let id = breakpoint.id;
        let locations = self.add_breakpoint(ctx, breakpoint);
        Ok(json!({ "breakpointId": id.to_string(), "locations": locations }))
    }

    fn set_breakpoint(&mut self, ctx: Option<&Ctx<'_>>, p: SetBreakpointParams) -> CdpResult {
        let script_id = p.location.script()?;
        let script = self.script(script_id)?;
        let position = p.location.position();
        let location = Where::Script {
            url: script.url.clone(),
            name: script.name.clone(),
        };
        let breakpoint = Breakpoint::new(
            self.fresh_breakpoint_id(),
            location,
            position,
            condition(p.condition),
        );
        let id = breakpoint.id;
        let mut locations = self.add_breakpoint(ctx, breakpoint);
        let actual = locations
            .pop()
            .unwrap_or_else(|| position.location(script_id));
        Ok(json!({ "breakpointId": id.to_string(), "actualLocation": actual }))
    }

    fn continue_to(&mut self, ctx: Option<&Ctx<'_>>, location: &Location) -> Result<(), CdpError> {
        let script = self.script(location.script()?)?;
        let at = Where::Script {
            url: script.url.clone(),
            name: script.name.clone(),
        };
        let breakpoint =
            Breakpoint::new(self.fresh_breakpoint_id(), at, location.position(), None).once();
        self.add_breakpoint(ctx, breakpoint);
        Ok(())
    }

    fn possible_breakpoints(
        &mut self,
        ctx: &Ctx<'_>,
        p: GetPossibleBreakpointsParams,
    ) -> CdpResult {
        let script_id = p.start.script()?;
        let script = self.script(script_id)?;
        let from = p.start.position();
        let to = p
            .end
            .as_ref()
            .map_or(Position::new(u32::MAX, u32::MAX), Location::position);
        let locations: Vec<Json> = engine::script_positions(ctx, &script.name)
            .into_iter()
            .filter(|&position| position >= from && position < to)
            .map(|position| position.location(script_id))
            .collect();
        Ok(json!({ "locations": locations }))
    }

    fn evaluate_on_call_frame(&mut self, ctx: &Ctx<'_>, p: EvaluateOnCallFrameParams) -> CdpResult {
        let frame = self.frame_index(&p.call_frame_id)?;
        let result = if p.expression.trim() == "this" {
            Ok(super::pause::frame_this(ctx, frame))
        } else {
            self.evaluate_in_frame(ctx, frame, &p.expression)
        };
        Ok(self.evaluation(ctx, result, &p.result))
    }

    fn set_variable_value(&mut self, ctx: &Ctx<'_>, p: SetVariableValueParams) -> CdpResult {
        let frame = self.frame_index(&p.call_frame_id)?;
        let value = self.argument(ctx, &p.new_value)?;
        match engine::set_variable(ctx, frame, &p.variable_name, &value) {
            SetVariable::Set => Ok(json!({})),
            SetVariable::NoSuchVariable => Err(CdpError::invalid_params(format!(
                "no variable '{}' in that frame",
                p.variable_name
            ))),
            SetVariable::Failed => Err(CdpError::server("could not set the variable")),
        }
    }
}
