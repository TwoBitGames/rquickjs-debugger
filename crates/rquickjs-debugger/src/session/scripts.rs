use std::cell::RefCell;
use std::rc::Rc;

use rquickjs::Ctx;
use serde_json::{Value as Json, json};

use super::Session;
use crate::protocol::{CdpError, ScriptId};
use crate::url::script_url;

#[derive(Clone, Debug, Default)]
pub struct ScriptLog(Rc<RefCell<Vec<Recorded>>>);

type Recorded = (String, Rc<str>);

impl ScriptLog {
    pub fn new() -> ScriptLog {
        ScriptLog::default()
    }

    pub fn record(&self, name: impl Into<String>, source: impl Into<Rc<str>>) {
        self.0.borrow_mut().push((name.into(), source.into()));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    fn take(&self) -> Vec<Recorded> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

#[derive(Debug)]
pub(super) struct Script {
    pub id: ScriptId,
    pub name: String,
    pub url: String,
    pub source: Rc<str>,
    end_line: u32,
    end_column: u32,
    pub announced: bool,
}

impl Script {
    fn new(id: ScriptId, name: String, source: Rc<str>) -> Script {
        let (mut end_line, mut end_column) = (0u32, 0u32);
        for line in source.split('\n') {
            end_line += 1;
            end_column = line.chars().count() as u32;
        }
        Script {
            id,
            url: script_url(&name),
            name,
            source,
            end_line: end_line.saturating_sub(1),
            end_column,
            announced: false,
        }
    }

    pub fn parsed_event(&self, context_id: u64) -> Json {
        json!({
            "scriptId": self.id.to_string(),
            "url": self.url,
            "startLine": 0,
            "startColumn": 0,
            "endLine": self.end_line,
            "endColumn": self.end_column,
            "executionContextId": context_id,
            "hash": "",
            "isModule": self.is_module(),
            "length": self.source.len(),
            "embedderName": self.url,
        })
    }

    fn is_module(&self) -> bool {
        self.name.starts_with('/') || self.name.get(1..2) == Some(":")
    }
}

impl Session {
    pub(super) fn script(&self, id: ScriptId) -> Result<&Script, CdpError> {
        self.scripts
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| CdpError::invalid_params("no such script"))
    }

    pub(super) fn script_by_name(&self, name: &str) -> Option<&Script> {
        self.scripts.iter().find(|s| s.name == name)
    }

    pub(super) fn flush_scripts(&mut self, ctx: Option<&Ctx<'_>>) {
        let Some(attached) = &self.attached else {
            return;
        };
        for (name, source) in attached.scripts.take() {
            if self.script_by_name(&name).is_some() {
                continue;
            }
            let script = Script::new(ScriptId(self.fresh_id()), name, source);
            self.scripts.push(script);
            let index = self.scripts.len() - 1;
            if self.debugger_enabled && self.client.is_some() {
                self.scripts[index].announced = true;
                let params = self.scripts[index].parsed_event(self.context_id);
                self.event("Debugger.scriptParsed", params);
            }
            if let Some(ctx) = ctx {
                self.resolve_against_script(ctx, index);
            }
        }
    }
}
