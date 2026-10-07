use rquickjs::Ctx;
use serde_json::json;
use tracing::{debug, trace};

use super::{Flow, Session};
use crate::protocol::{CdpError, Request};

impl Session {
    pub(super) fn handle(&mut self, ctx: Option<&Ctx<'_>>, text: &str) -> Flow {
        trace!(target: "cdp", "{} <- {text}", self.name);
        let request = match Request::parse(text) {
            Ok(request) => request,
            Err(error) => {
                debug!("debugger '{}': unreadable message: {error}", self.name);
                self.send(&json!({ "id": null, "error": error.to_json() }));
                return Flow::Stay;
            }
        };
        let mut flow = Flow::Stay;
        let result = match request.domain() {
            "Runtime" => self.runtime_domain(ctx, &request.method, &request.params),
            "Debugger" => self.debugger_domain(ctx, &request.method, &request.params, &mut flow),
            _ => Ok(json!({})),
        };
        match result {
            Ok(result) => self.send(&json!({ "id": request.id, "result": result })),
            Err(error) => {
                debug!(
                    "debugger '{}': {} failed: {error}",
                    self.name, request.method
                );
                self.send(&json!({ "id": request.id, "error": error.to_json() }));
            }
        }
        for event in std::mem::take(&mut self.deferred) {
            self.send(&event);
        }
        flow
    }

    pub(super) fn need_ctx<'a, 'js>(ctx: Option<&'a Ctx<'js>>) -> Result<&'a Ctx<'js>, CdpError> {
        ctx.ok_or_else(CdpError::no_runtime)
    }

    pub(super) fn need_pause(&self) -> Result<(), CdpError> {
        if self.paused {
            Ok(())
        } else {
            Err(CdpError::not_paused())
        }
    }
}
