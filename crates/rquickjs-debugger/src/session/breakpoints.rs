use rquickjs::{Ctx, Function};
use serde_json::Value as Json;

use super::Session;
use crate::engine;
use crate::protocol::{BreakpointId, CdpError, Position};
use crate::url::script_name;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Where {
    Url(String),
    UrlRegex(String),
    Script { url: String, name: String },
}

#[derive(Clone, Debug)]
pub(super) struct Breakpoint {
    pub id: BreakpointId,
    location: Where,
    file: Option<String>,
    requested: Position,
    condition: Option<String>,
    resolved: Option<Position>,
    registered: Option<(String, u32)>,
    once: bool,
}

impl Breakpoint {
    pub fn new(
        id: BreakpointId,
        location: Where,
        requested: Position,
        condition: Option<String>,
    ) -> Breakpoint {
        let file = match &location {
            Where::Url(url) => Some(script_name(url)).filter(|name| !name.is_empty()),
            Where::UrlRegex(_) => None,
            Where::Script { name, .. } => Some(name.clone()),
        };
        Breakpoint {
            id,
            location,
            file,
            requested,
            condition,
            resolved: None,
            registered: None,
            once: false,
        }
    }

    pub fn once(mut self) -> Breakpoint {
        self.once = true;
        self
    }

    pub fn forget_runtime(&mut self) {
        self.resolved = None;
        self.registered = None;
    }

    fn effective_line(&self) -> u32 {
        self.resolved.map_or(self.requested.line, |p| p.line)
    }
}

impl Session {
    pub(super) fn fresh_breakpoint_id(&mut self) -> BreakpointId {
        BreakpointId(self.fresh_id())
    }

    pub(super) fn add_breakpoint(
        &mut self,
        ctx: Option<&Ctx<'_>>,
        breakpoint: Breakpoint,
    ) -> Vec<Json> {
        self.breakpoints.push(breakpoint);
        let index = self.breakpoints.len() - 1;
        if let Some(ctx) = ctx {
            self.resolve_breakpoint(ctx, index);
            self.register_breakpoint(ctx, index);
        }
        self.breakpoint_locations(ctx, index)
    }

    pub(super) fn remove_breakpoint(&mut self, ctx: Option<&Ctx<'_>>, id: BreakpointId) {
        if let Some(index) = self.breakpoints.iter().position(|b| b.id == id) {
            self.unregister_breakpoint(ctx, index);
            self.breakpoints.remove(index);
        }
    }

    pub(super) fn clear_breakpoints(&mut self, ctx: Option<&Ctx<'_>>) {
        for index in 0..self.breakpoints.len() {
            self.unregister_breakpoint(ctx, index);
        }
        self.breakpoints.clear();
    }

    pub(super) fn set_breakpoints_active(&mut self, active: bool) {
        self.breakpoints_active = active;
        if let Some(runtime) = self.runtime() {
            runtime.set_breakpoints_active(active);
        }
    }

    pub(super) fn set_pause_on_exceptions(&mut self, on: bool) {
        self.pause_on_exceptions = on;
        if let Some(runtime) = self.runtime() {
            runtime.set_pause_on_exceptions(on);
        }
    }

    fn breakpoint_locations(&mut self, ctx: Option<&Ctx<'_>>, index: usize) -> Vec<Json> {
        let Some(script) = self.script_of_breakpoint(ctx, index) else {
            return Vec::new();
        };
        let script_id = self.scripts[script].id;
        self.breakpoints[index]
            .resolved
            .map(|position| vec![position.location(script_id)])
            .unwrap_or_default()
    }

    fn script_of_breakpoint(&mut self, ctx: Option<&Ctx<'_>>, index: usize) -> Option<usize> {
        (0..self.scripts.len()).find(|&script| self.breakpoint_matches(ctx, index, script))
    }

    pub(super) fn breakpoint_matches(
        &mut self,
        ctx: Option<&Ctx<'_>>,
        index: usize,
        script: usize,
    ) -> bool {
        let (breakpoint, script) = (&self.breakpoints[index], &self.scripts[script]);
        match &breakpoint.location {
            Where::Url(url) => {
                script.url == *url || breakpoint.file.as_deref() == Some(script.name.as_str())
            }
            Where::Script { name, .. } => script.name == *name,
            Where::UrlRegex(regex) => {
                let Some(ctx) = ctx else {
                    return false;
                };
                let (regex, url) = (regex.clone(), script.url.clone());
                self.helper(ctx)
                    .and_then(|helper| helper.get::<_, Function<'_>>("matchUrl"))
                    .and_then(|match_url| {
                        self.without_pausing(|| match_url.call::<_, bool>((regex, url)))
                    })
                    .unwrap_or(false)
            }
        }
    }

    pub(super) fn check_regex(&mut self, ctx: &Ctx<'_>, source: &str) -> Result<(), CdpError> {
        let check: Function<'_> = self.helper(ctx)?.get("checkRegex")?;
        self.without_pausing(|| check.call::<_, ()>((source,)))
            .map_err(|error| {
                let why = if error.is_exception() {
                    super::values::pending_exception_message(ctx)
                } else {
                    error.to_string()
                };
                CdpError::invalid_params(format!("urlRegex: {why}"))
            })
    }

    pub(super) fn resolve_breakpoint(&mut self, ctx: &Ctx<'_>, index: usize) {
        let Some(script) = self.script_of_breakpoint(Some(ctx), index) else {
            return;
        };
        let name = self.scripts[script].name.clone();
        let positions = engine::script_positions(ctx, &name);
        let breakpoint = &mut self.breakpoints[index];
        let requested = breakpoint.requested;
        breakpoint.file = Some(name);
        breakpoint.resolved = positions
            .iter()
            .copied()
            .filter(|p| p.line == requested.line && p.column >= requested.column)
            .min()
            .or_else(|| {
                positions
                    .iter()
                    .copied()
                    .filter(|p| p.line > requested.line)
                    .min()
            });
    }

    pub(super) fn register_breakpoint(&mut self, ctx: &Ctx<'_>, index: usize) {
        let breakpoint = &self.breakpoints[index];
        let Some(file) = breakpoint.file.clone() else {
            return;
        };
        let want = (file, breakpoint.effective_line());
        if breakpoint.registered.as_ref() == Some(&want) {
            return;
        }
        self.unregister_breakpoint(Some(ctx), index);
        if engine::set_breakpoint(ctx, &want.0, want.1) {
            self.breakpoints[index].registered = Some(want);
        }
    }

    pub(super) fn unregister_breakpoint(&mut self, ctx: Option<&Ctx<'_>>, index: usize) {
        let Some(registration) = self.breakpoints[index].registered.take() else {
            return;
        };
        let Some(ctx) = ctx else {
            return;
        };
        let shared = self
            .breakpoints
            .iter()
            .enumerate()
            .any(|(other, b)| other != index && b.registered.as_ref() == Some(&registration));
        if !shared {
            engine::remove_breakpoint(ctx, &registration.0, registration.1);
        }
    }

    pub(super) fn resolve_against_script(&mut self, ctx: &Ctx<'_>, script: usize) {
        for index in 0..self.breakpoints.len() {
            if self.breakpoints[index].resolved.is_some()
                || !self.breakpoint_matches(Some(ctx), index, script)
            {
                continue;
            }
            self.resolve_breakpoint(ctx, index);
            self.register_breakpoint(ctx, index);
            if let Some(position) = self.breakpoints[index].resolved {
                let id = self.breakpoints[index].id;
                let script_id = self.scripts[script].id;
                self.event(
                    "Debugger.breakpointResolved",
                    serde_json::json!({ "breakpointId": id.to_string(), "location": position.location(script_id) }),
                );
            }
        }
    }

    pub(super) fn breakpoints_hit(
        &mut self,
        ctx: &Ctx<'_>,
        file: &str,
        line: u32,
    ) -> Vec<BreakpointId> {
        let at = (file.to_string(), line);
        let candidates: Vec<usize> = (0..self.breakpoints.len())
            .filter(|&i| self.breakpoints[i].registered.as_ref() == Some(&at))
            .collect();
        let mut hit = Vec::new();
        for index in candidates {
            let holds = match self.breakpoints[index].condition.clone() {
                None => true,
                Some(condition) => self
                    .evaluate_in_frame(ctx, 0, &condition)
                    .is_ok_and(|v| super::values::truthy(&v)),
            };
            if holds {
                hit.push(self.breakpoints[index].id);
            }
        }
        let one_shot: Vec<usize> = (0..self.breakpoints.len())
            .rev()
            .filter(|&i| self.breakpoints[i].once && hit.contains(&self.breakpoints[i].id))
            .collect();
        for index in one_shot {
            self.unregister_breakpoint(Some(ctx), index);
            self.breakpoints.remove(index);
        }
        hit
    }
}
