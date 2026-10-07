use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::{Value as Json, json};

#[derive(Debug)]
pub(crate) struct Request {
    pub id: Option<Json>,
    pub method: String,
    pub params: Json,
}

impl Request {
    pub fn parse(text: &str) -> Result<Request, CdpError> {
        #[derive(Deserialize)]
        struct Wire {
            id: Option<Json>,
            method: String,
            #[serde(default)]
            params: Json,
        }
        let wire: Wire = serde_json::from_str(text).map_err(|e| CdpError {
            code: CdpError::PARSE,
            message: e.to_string(),
        })?;
        Ok(Request {
            id: wire.id,
            method: wire.method,
            params: wire.params,
        })
    }

    pub fn domain(&self) -> &str {
        self.method.split_once('.').map_or("", |(domain, _)| domain)
    }
}

pub(crate) fn params<T: DeserializeOwned>(params: &Json) -> Result<T, CdpError> {
    serde_json::from_value(params.clone()).map_err(|e| CdpError::invalid_params(e.to_string()))
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct CdpError {
    pub code: i64,
    pub message: String,
}

impl CdpError {
    pub const PARSE: i64 = -32700;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const SERVER: i64 = -32000;

    pub fn invalid_params(message: impl Into<String>) -> Self {
        CdpError {
            code: Self::INVALID_PARAMS,
            message: message.into(),
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        CdpError {
            code: Self::METHOD_NOT_FOUND,
            message: format!("'{method}' wasn't found"),
        }
    }

    pub fn server(message: impl Into<String>) -> Self {
        CdpError {
            code: Self::SERVER,
            message: message.into(),
        }
    }

    pub fn not_paused() -> Self {
        Self::server("Can only perform operation while paused.")
    }

    pub fn no_runtime() -> Self {
        Self::server("no runtime is attached")
    }

    pub fn unsupported() -> Self {
        Self::server("not supported by this runtime")
    }

    pub fn to_json(&self) -> Json {
        json!({ "code": self.code, "message": self.message })
    }
}

impl From<rquickjs::Error> for CdpError {
    fn from(e: rquickjs::Error) -> Self {
        CdpError::server(e.to_string())
    }
}

pub(crate) type CdpResult = Result<Json, CdpError>;

macro_rules! numeric_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(crate) struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = CdpError;

            fn from_str(s: &str) -> Result<Self, CdpError> {
                s.parse()
                    .map($name)
                    .map_err(|_| CdpError::invalid_params(format!("invalid {}", stringify!($name))))
            }
        }
    };
}

numeric_id!(ScriptId);
numeric_id!(BreakpointId);
numeric_id!(RemoteObjectId);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CallFrameId {
    pub pause: u64,
    pub frame: u32,
}

impl fmt::Display for CallFrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.pause, self.frame)
    }
}

impl FromStr for CallFrameId {
    type Err = CdpError;

    fn from_str(s: &str) -> Result<Self, CdpError> {
        let invalid = || CdpError::invalid_params("invalid callFrameId");
        let (pause, frame) = s.split_once(':').ok_or_else(invalid)?;
        Ok(CallFrameId {
            pause: pause.parse().map_err(|_| invalid())?,
            frame: frame.parse().map_err(|_| invalid())?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub const fn new(line: u32, column: u32) -> Self {
        Position { line, column }
    }

    pub fn from_protocol(line: u64, column: Option<u64>) -> Self {
        Position {
            line: line as u32 + 1,
            column: column.map_or(1, |c| c as u32 + 1),
        }
    }

    pub fn location(self, script: ScriptId) -> Json {
        json!({
            "scriptId": script.to_string(),
            "lineNumber": self.line - 1,
            "columnNumber": self.column - 1,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Location {
    pub script_id: String,
    pub line_number: u64,
    pub column_number: Option<u64>,
}

impl Location {
    pub fn script(&self) -> Result<ScriptId, CdpError> {
        self.script_id.parse()
    }

    pub fn position(&self) -> Position {
        Position::from_protocol(self.line_number, self.column_number)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ResultOptions {
    pub return_by_value: bool,
    pub generate_preview: bool,
    pub object_group: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CallArgument {
    /// `None` only when the key is absent: an explicit JSON `null` is a
    /// JavaScript `null` argument, not a missing one.
    #[serde(deserialize_with = "present")]
    pub value: Option<Json>,
    pub unserializable_value: Option<String>,
    pub object_id: Option<String>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvaluateParams {
    pub expression: String,
    #[serde(flatten)]
    pub result: ResultOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallFunctionOnParams {
    pub function_declaration: String,
    pub object_id: Option<String>,
    #[serde(default)]
    pub arguments: Vec<CallArgument>,
    #[serde(flatten)]
    pub result: ResultOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetPropertiesParams {
    pub object_id: String,
    #[serde(default)]
    pub accessor_properties_only: bool,
    #[serde(default)]
    pub generate_preview: bool,
    #[serde(default)]
    pub object_group: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObjectParams {
    pub object_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObjectGroupParams {
    pub object_group: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetBreakpointsActiveParams {
    pub active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetSkipAllPausesParams {
    pub skip: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetPauseOnExceptionsParams {
    pub state: PauseOnExceptions,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PauseOnExceptions {
    None,
    Caught,
    Uncaught,
    All,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetBreakpointByUrlParams {
    pub line_number: u64,
    pub column_number: Option<u64>,
    pub url: Option<String>,
    pub url_regex: Option<String>,
    pub condition: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetBreakpointParams {
    pub location: Location,
    pub condition: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveBreakpointParams {
    pub breakpoint_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetPossibleBreakpointsParams {
    pub start: Location,
    pub end: Option<Location>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetScriptSourceParams {
    pub script_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContinueToLocationParams {
    pub location: Location,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvaluateOnCallFrameParams {
    pub call_frame_id: String,
    pub expression: String,
    #[serde(flatten)]
    pub result: ResultOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetVariableValueParams {
    pub call_frame_id: String,
    pub variable_name: String,
    pub new_value: CallArgument,
}

pub(crate) fn condition(raw: Option<String>) -> Option<String> {
    raw.filter(|c| !c.trim().is_empty())
}
