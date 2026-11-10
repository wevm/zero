use zero_client::json::{Object, Value};

/// How a case matched JS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Exactly.
    Pass,
    /// Only under the named documented exception (`rust/DESIGN.md`).
    Exception(&'static str),
}

/// Reading a case's fields, with errors that name the missing field.
pub trait Case {
    fn field(&self, key: &str) -> Result<&Value, String>;
    fn str(&self, key: &str) -> Result<&str, String>;
    fn num(&self, key: &str) -> Result<f64, String>;
    fn bool(&self, key: &str) -> Result<bool, String>;
    fn arr(&self, key: &str) -> Result<&Vec<Value>, String>;
    fn obj(&self, key: &str) -> Result<&Object, String>;
}

fn want<T>(v: Option<T>, key: &str, kind: &str, case: &Value) -> Result<T, String> {
    v.ok_or_else(|| {
        let text: String = case.to_string().chars().take(200).collect();
        format!("vector field `{key}` is not {kind} in {text}")
    })
}

impl Case for Value {
    fn field(&self, key: &str) -> Result<&Value, String> {
        want(self.get(key), key, "present", self)
    }
    fn str(&self, key: &str) -> Result<&str, String> {
        want(self.get(key).and_then(Value::as_str), key, "a string", self)
    }
    fn num(&self, key: &str) -> Result<f64, String> {
        want(self.get(key).and_then(Value::as_f64), key, "a number", self)
    }
    fn bool(&self, key: &str) -> Result<bool, String> {
        want(
            self.get(key).and_then(Value::as_bool),
            key,
            "a boolean",
            self,
        )
    }
    fn arr(&self, key: &str) -> Result<&Vec<Value>, String> {
        want(
            self.get(key).and_then(Value::as_array),
            key,
            "an array",
            self,
        )
    }
    fn obj(&self, key: &str) -> Result<&Object, String> {
        want(
            self.get(key).and_then(Value::as_object),
            key,
            "an object",
            self,
        )
    }
}

/// `Ok(())` when `ok`, else the message.
pub fn ensure(ok: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message()) }
}

/// Fails with "Rust <got>, JS <want>" unless they're equal.
pub fn same<T: PartialEq + std::fmt::Debug>(
    what: impl std::fmt::Display,
    got: T,
    want: T,
) -> Result<(), String> {
    ensure(got == want, || format!("{what}: Rust {got:?}, JS {want:?}"))
}

/// The JS `debugDescription`-ish rendering of a string, for messages.
pub fn quote(s: &str) -> String {
    zero_client::json::stringify(&Value::String(s.to_owned()))
}
