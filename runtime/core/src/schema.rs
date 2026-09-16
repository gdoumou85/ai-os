/// JSON schema for one `Move`, sent to the runner as `format` (decision 13). Mirrors moves.rs
/// and executor::action::Action; a new move or action kind must be added in both places.
///
/// Key order below is load-bearing: the runner's grammar makes the model commit to an object by
/// its *first* key, so the discriminator (`move` here, `kind` in `$defs.action`) must be written
/// first in every `properties` object. `serde_json`'s `preserve_order` feature (see core's
/// Cargo.toml) is what keeps `value()` honouring this order instead of alphabetising it — without
/// it, `start`'s `creative` would sort before `move` and lock a small model into `reply` (the only
/// variant whose alphabetical-first key happens to be `move`).
pub const MOVE_SCHEMA: &str = r##"{
  "$defs": {
    "action": { "oneOf": [
      { "type":"object", "properties": { "kind": {"enum":["run_command"]}, "argv": {"type":"array","items":{"type":"string"},"minItems":1} }, "required":["kind","argv"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["read_file"]}, "path": {"type":"string"}, "from_line": {"type":"integer","minimum":1}, "lines": {"type":"integer","minimum":1} }, "required":["kind","path"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["write_file"]}, "path": {"type":"string"}, "contents": {"type":"string"} }, "required":["kind","path","contents"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["edit_file"]}, "path": {"type":"string"}, "find": {"type":"string"}, "replace": {"type":"string"} }, "required":["kind","path","find","replace"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["http_post"]}, "url": {"type":"string"}, "body": {"type":"string"} }, "required":["kind","url","body"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["install"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","packages"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["remove"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","packages"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["service"]}, "name": {"type":"string"}, "do": {"enum":["enable","disable","restart"]} }, "required":["kind","name","do"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["make_dir"]}, "path": {"type":"string"} }, "required":["kind","path"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["fetch_packages"]}, "manager": {"enum":["pip","npm","cargo"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","manager","packages"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["set_setting"]}, "key": {"enum":["projects_root"]}, "value": {"type":"string"} }, "required":["kind","key","value"], "additionalProperties": false }
    ] }
  },
  "oneOf": [
    { "type":"object", "properties": { "move": {"enum":["reply"]}, "text": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","text"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["start"]}, "project": {"type":"string"}, "new_project": {"type":"boolean"}, "description": {"type":"string"}, "goal": {"type":"string"}, "creative": {"type":"boolean"}, "understood": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","project","new_project","description","goal","creative","understood"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["housekeep"]}, "goal": {"type":"string"}, "understood": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","goal","understood"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["ask"]}, "questions": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":3} }, "required":["move","questions"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["plan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8} }, "required":["move","steps"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["act"]}, "step": {"type":"integer","minimum":1}, "action": {"$ref":"#/$defs/action"} }, "required":["move","step","action"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["replan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8}, "why": {"type":"string"} }, "required":["move","steps","why"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["done"]}, "summary": {"type":"string"}, "check": {"$ref":"#/$defs/action"} }, "required":["move","summary","check"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["give_up"]}, "reason": {"type":"string"}, "missing": {"type":"string"} }, "required":["move","reason","missing"], "additionalProperties": false }
  ]
}"##;

pub fn value() -> serde_json::Value {
    serde_json::from_str(MOVE_SCHEMA).expect("MOVE_SCHEMA is valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner's grammar commits to an object by its first key (decision 13, round 2): without
    /// `serde_json`'s `preserve_order` feature, `Value::Object` would alphabetise `properties`
    /// and `start`'s `creative` would sort ahead of `move`, locking a small model into `reply`
    /// (the only variant whose alphabetically-first key is `move`). This test fails on plain
    /// `serde_json` and passes with `preserve_order`.
    #[test]
    fn move_is_always_the_first_key() {
        let schema = value();
        for entry in schema["oneOf"].as_array().unwrap() {
            let props = entry["properties"].as_object().unwrap();
            let first = props.keys().next().unwrap();
            assert_eq!(first, "move", "discriminator must be written first: {entry}");
        }
        for entry in schema["$defs"]["action"]["oneOf"].as_array().unwrap() {
            let props = entry["properties"].as_object().unwrap();
            let first = props.keys().next().unwrap();
            assert_eq!(first, "kind", "discriminator must be written first: {entry}");
        }
    }

    /// A new `Action` variant (executor::action) must be taught to the grammar here, in the same
    /// order and under the same names — this pins the full $defs.action.oneOf list against silent
    /// drift when a kind is added to one side and not the other.
    #[test]
    fn action_kinds_match_the_enum() {
        let v = value();
        let kinds: Vec<String> = v["$defs"]["action"]["oneOf"].as_array().unwrap().iter()
            .map(|o| o["properties"]["kind"]["enum"][0].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, [
            "run_command", "read_file", "write_file", "edit_file", "http_post",
            "install", "remove", "service", "make_dir", "fetch_packages", "set_setting",
        ]);
    }
}
