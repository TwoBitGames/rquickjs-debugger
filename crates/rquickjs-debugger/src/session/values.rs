use std::collections::BTreeMap;

use rquickjs::context::EvalOptions;
use rquickjs::{Ctx, Function, Object, Persistent, Value};
use serde_json::{Value as Json, json};

use super::Session;
use crate::protocol::{CallArgument, CdpError, RemoteObjectId, ResultOptions};

const HELPER_SOURCE: &str = include_str!("../inspect.js");
const HELPER_FILENAME: &str = "<rquickjs-debugger>";

const MAX_REMOTE_OBJECTS: usize = 20_000;

#[derive(Default)]
pub(super) struct RemoteObjects {
    handles: BTreeMap<RemoteObjectId, (Persistent<Value<'static>>, String)>,
}

impl RemoteObjects {
    fn insert(&mut self, id: RemoteObjectId, value: Persistent<Value<'static>>, group: &str) {
        self.handles.insert(id, (value, group.to_string()));
        while self.handles.len() > MAX_REMOTE_OBJECTS {
            self.handles.pop_first();
        }
    }

    fn get(&self, id: RemoteObjectId) -> Option<&Persistent<Value<'static>>> {
        self.handles.get(&id).map(|(value, _)| value)
    }

    pub fn release(&mut self, id: RemoteObjectId) {
        self.handles.remove(&id);
    }

    pub fn release_group(&mut self, group: &str) {
        self.handles.retain(|_, (_, g)| g != group);
    }

    pub fn clear(&mut self) {
        self.handles.clear();
    }
}

impl Session {
    pub(super) fn helper<'js>(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
        let attached = self.attached.as_mut().ok_or(rquickjs::Error::Unknown)?;
        if let Some(helper) = &attached.helper {
            return helper.clone().restore(ctx);
        }
        let mut options = EvalOptions::default();
        options.global = true;
        options.filename = Some(HELPER_FILENAME.into());
        let helper: Object<'js> = ctx.eval_with_options(HELPER_SOURCE, options)?;
        attached.helper = Some(Persistent::save(ctx, helper.clone()));
        Ok(helper)
    }

    pub(super) fn remote<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        value: Value<'js>,
        preview: bool,
        group: &str,
    ) -> Json {
        let described = self
            .helper(ctx)
            .and_then(|helper| helper.get::<_, Function<'js>>("describe"))
            .and_then(|describe| {
                self.without_pausing(|| describe.call::<_, String>((value.clone(), preview)))
            });
        let mut object = match described {
            Ok(text) => serde_json::from_str(&text).unwrap_or(json!({ "type": "undefined" })),
            Err(error) => json!({ "type": "string", "value": format!("<unreadable: {error}>") }),
        };
        if value.is_object() || value.is_symbol() {
            let id = RemoteObjectId(self.fresh_id());
            self.objects.insert(id, Persistent::save(ctx, value), group);
            object["objectId"] = json!(id.to_string());
        }
        object
    }

    pub(super) fn object<'js>(&self, ctx: &Ctx<'js>, id: &str) -> Result<Value<'js>, CdpError> {
        let id: RemoteObjectId = id.parse()?;
        let handle = self
            .objects
            .get(id)
            .ok_or_else(|| CdpError::invalid_params("Could not find object with given id"))?;
        Ok(handle.clone().restore(ctx)?)
    }

    pub(super) fn argument<'js>(
        &self,
        ctx: &Ctx<'js>,
        argument: &CallArgument,
    ) -> Result<Value<'js>, CdpError> {
        if let Some(id) = &argument.object_id {
            return self.object(ctx, id);
        }
        if let Some(unserializable) = &argument.unserializable_value {
            let mut options = EvalOptions::default();
            options.global = true;
            let source = unserializable.clone().into_bytes();
            return Ok(
                self.without_pausing(|| ctx.eval_with_options::<Value<'js>, _>(source, options))?
            );
        }
        if let Some(value) = &argument.value {
            return Ok(ctx.json_parse(value.to_string())?);
        }
        Ok(Value::new_undefined(ctx.clone()))
    }

    pub(super) fn evaluation<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        result: rquickjs::Result<Value<'js>>,
        options: &ResultOptions,
    ) -> Json {
        match result {
            Ok(value) => {
                let result = if options.return_by_value {
                    self.by_value(ctx, value)
                } else {
                    self.remote(ctx, value, options.generate_preview, &options.object_group)
                };
                json!({ "result": result })
            }
            Err(error) => {
                let details = self.exception_details(ctx, &error, &options.object_group);
                json!({ "result": details["exception"].clone(), "exceptionDetails": details })
            }
        }
    }

    fn by_value<'js>(&mut self, ctx: &Ctx<'js>, value: Value<'js>) -> Json {
        let text = self
            .helper(ctx)
            .and_then(|helper| helper.get::<_, Function<'js>>("toJson"))
            .and_then(|to_json| {
                self.without_pausing(|| to_json.call::<_, Option<String>>((value.clone(),)))
            });
        let mut object = self.remote(ctx, value, false, "");
        if let Some(fields) = object.as_object_mut() {
            fields.remove("objectId");
        }
        if let Ok(Some(text)) = text {
            if let Ok(parsed) = serde_json::from_str::<Json>(&text) {
                object["value"] = parsed;
            }
        }
        object
    }

    fn exception_details(&mut self, ctx: &Ctx<'_>, error: &rquickjs::Error, group: &str) -> Json {
        let id = self.fresh_id();
        let (exception, text) = if error.is_exception() {
            let thrown = ctx.catch();
            let text = error_message(&thrown).unwrap_or_else(|| "Uncaught".into());
            (self.remote(ctx, thrown, false, group), text)
        } else {
            (
                json!({ "type": "string", "value": error.to_string() }),
                error.to_string(),
            )
        };
        json!({
            "exceptionId": id,
            "text": format!("Uncaught {text}"),
            "lineNumber": 0,
            "columnNumber": 0,
            "executionContextId": self.context_id,
            "exception": exception,
        })
    }
}

pub(super) fn pending_exception_message(ctx: &Ctx<'_>) -> String {
    error_message(&ctx.catch()).unwrap_or_default()
}

fn error_message(thrown: &Value<'_>) -> Option<String> {
    thrown.as_object()?.get::<_, String>("message").ok()
}

pub(super) fn truthy(value: &Value<'_>) -> bool {
    if value.is_undefined() || value.is_null() {
        return false;
    }
    if let Some(b) = value.as_bool() {
        return b;
    }
    if let Some(n) = value.as_number() {
        return n != 0.0 && !n.is_nan();
    }
    if let Some(s) = value.as_string() {
        return !s.to_string().unwrap_or_default().is_empty();
    }
    true
}
