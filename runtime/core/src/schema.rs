/// JSON schema for one `Move`, sent to the runner as `format` (decision 13). Mirrors moves.rs
/// and executor::action::Action; a new move or action kind must be added in both places.
pub const MOVE_SCHEMA: &str = r##"{
  "$defs": {
    "action": { "oneOf": [
      { "type":"object", "properties": { "kind": {"enum":["run_command"]}, "argv": {"type":"array","items":{"type":"string"},"minItems":1} }, "required":["kind","argv"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["read_file"]}, "path": {"type":"string"}, "from_line": {"type":"integer"}, "lines": {"type":"integer"} }, "required":["kind","path"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["write_file"]}, "path": {"type":"string"}, "contents": {"type":"string"} }, "required":["kind","path","contents"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["edit_file"]}, "path": {"type":"string"}, "find": {"type":"string"}, "replace": {"type":"string"} }, "required":["kind","path","find","replace"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["http_post"]}, "url": {"type":"string"}, "body": {"type":"string"} }, "required":["kind","url","body"], "additionalProperties": false }
    ] }
  },
  "oneOf": [
    { "type":"object", "properties": { "move": {"enum":["reply"]}, "text": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","text"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["start"]}, "project": {"type":"string"}, "new_project": {"type":"boolean"}, "description": {"type":"string"}, "goal": {"type":"string"}, "creative": {"type":"boolean"}, "understood": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","project","new_project","description","goal","creative","understood"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["ask"]}, "questions": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":3} }, "required":["move","questions"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["plan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8} }, "required":["move","steps"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["act"]}, "step": {"type":"integer"}, "action": {"$ref":"#/$defs/action"} }, "required":["move","step","action"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["replan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8}, "why": {"type":"string"} }, "required":["move","steps","why"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["done"]}, "summary": {"type":"string"}, "check": {"$ref":"#/$defs/action"} }, "required":["move","summary","check"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["give_up"]}, "reason": {"type":"string"}, "missing": {"type":"string"} }, "required":["move","reason","missing"], "additionalProperties": false }
  ]
}"##;

pub fn value() -> serde_json::Value {
    serde_json::from_str(MOVE_SCHEMA).expect("MOVE_SCHEMA is valid JSON")
}
