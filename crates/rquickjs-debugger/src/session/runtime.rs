use rquickjs::context::EvalOptions;
use rquickjs::{Array, Ctx, Function, Value};
use serde_json::{Value as Json, json};

use super::Session;
use crate::protocol::{
    CallFunctionOnParams, CdpError, CdpResult, EvaluateParams, GetPropertiesParams,
    ReleaseObjectGroupParams, ReleaseObjectParams, params,
};

const CONSOLE_FILENAME: &str = "<console>";

impl Session {
    pub(super) fn runtime_domain(
        &mut self,
        ctx: Option<&Ctx<'_>>,
        method: &str,
        p: &Json,
    ) -> CdpResult {
        match method {
            "Runtime.enable" => {
                self.runtime_enabled = true;
                if self.attached.is_some() {
                    let context = self.context_json();
                    self.defer_event(
                        "Runtime.executionContextCreated",
                        json!({ "context": context }),
                    );
                }
                Ok(json!({}))
            }
            "Runtime.disable" => {
                self.runtime_enabled = false;
                Ok(json!({}))
            }
            "Runtime.evaluate" => Ok(self.evaluate(Self::need_ctx(ctx)?, params(p)?)),
            "Runtime.callFunctionOn" => self.call_function_on(Self::need_ctx(ctx)?, params(p)?),
            "Runtime.getProperties" => self.get_properties(Self::need_ctx(ctx)?, params(p)?),
            "Runtime.releaseObject" => {
                let p: ReleaseObjectParams = params(p)?;
                self.objects.release(p.object_id.parse()?);
                Ok(json!({}))
            }
            "Runtime.releaseObjectGroup" => {
                let p: ReleaseObjectGroupParams = params(p)?;
                self.objects.release_group(&p.object_group);
                Ok(json!({}))
            }
            "Runtime.getIsolateId" => Ok(json!({ "id": self.name })),
            "Runtime.getHeapUsage" => Ok(json!({ "usedSize": 0, "totalSize": 0 })),
            "Runtime.globalLexicalScopeNames" => Ok(json!({ "names": [] })),
            "Runtime.compileScript"
            | "Runtime.runIfWaitingForDebugger"
            | "Runtime.discardConsoleEntries"
            | "Runtime.setAsyncCallStackDepth"
            | "Runtime.setMaxCallStackSizeToCapture"
            | "Runtime.setCustomObjectFormatterEnabled"
            | "Runtime.addBinding"
            | "Runtime.removeBinding"
            | "Runtime.terminateExecution" => Ok(json!({})),
            _ => Err(CdpError::method_not_found(method)),
        }
    }

    fn evaluate(&mut self, ctx: &Ctx<'_>, p: EvaluateParams) -> Json {
        let result = self.without_pausing(|| {
            ctx.eval_with_options::<Value<'_>, _>(p.expression.into_bytes(), console_options())
        });
        self.evaluation(ctx, result, &p.result)
    }

    fn call_function_on(&mut self, ctx: &Ctx<'_>, p: CallFunctionOnParams) -> CdpResult {
        let this = match &p.object_id {
            Some(id) => self.object(ctx, id)?,
            None => Value::new_undefined(ctx.clone()),
        };
        let args = Array::new(ctx.clone())?;
        for (i, argument) in p.arguments.iter().enumerate() {
            args.set(i, self.argument(ctx, argument)?)?;
        }
        let call_on: Function<'_> = self.helper(ctx)?.get("callOn")?;
        let declaration = format!("({})", p.function_declaration).into_bytes();
        let result = self.without_pausing(|| {
            let function: Function<'_> = ctx.eval_with_options(declaration, console_options())?;
            call_on.call::<_, Value<'_>>((function, this, args))
        });
        Ok(self.evaluation(ctx, result, &p.result))
    }

    fn get_properties(&mut self, ctx: &Ctx<'_>, p: GetPropertiesParams) -> CdpResult {
        let target = self.object(ctx, &p.object_id)?;
        let props: Function<'_> = self.helper(ctx)?.get("props")?;
        let out: Array<'_> =
            self.without_pausing(|| props.call((target, p.accessor_properties_only)))?;
        let meta: Vec<Json> = serde_json::from_str(&out.get::<String>(0)?).unwrap_or_default();
        let values: Array<'_> = out.get(1)?;
        let getters: Array<'_> = out.get(2)?;
        let setters: Array<'_> = out.get(3)?;
        let prototype: Value<'_> = out.get(4)?;

        let mut result = Vec::with_capacity(meta.len());
        for (i, m) in meta.iter().enumerate() {
            let mut entry = json!({
                "name": m["name"],
                "enumerable": m["enumerable"],
                "writable": m["writable"],
                "configurable": m["configurable"],
                "isOwn": true,
            });
            if m["accessor"].as_bool().unwrap_or(false) {
                let getter: Value<'_> = getters.get(i)?;
                let setter: Value<'_> = setters.get(i)?;
                if !getter.is_undefined() {
                    entry["get"] = self.remote(ctx, getter, false, &p.object_group);
                }
                if !setter.is_undefined() {
                    entry["set"] = self.remote(ctx, setter, false, &p.object_group);
                }
            } else {
                let value: Value<'_> = values.get(i)?;
                entry["value"] = self.remote(ctx, value, p.generate_preview, &p.object_group);
            }
            result.push(entry);
        }

        let mut reply = json!({ "result": result });
        if !p.accessor_properties_only && !prototype.is_null() && !prototype.is_undefined() {
            let prototype = self.remote(ctx, prototype, false, &p.object_group);
            reply["internalProperties"] = json!([{ "name": "[[Prototype]]", "value": prototype }]);
        }
        Ok(reply)
    }
}

fn console_options() -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.filename = Some(CONSOLE_FILENAME.into());
    options
}
