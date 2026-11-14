//! [`Value`] and serde, behind the `serde` feature: [`to_value`] writes any
//! `Serialize` type as a value, [`from_value`] reads one back as any
//! `Deserialize` type, and `Value` and [`Object`] are both themselves.
//!
//! A struct's fields are written in the order they are declared, as a JS
//! object literal keeps its properties, and a JS-property-order object
//! keeps integer-like keys first. Numbers are `f64`, as in JS: an integer
//! field reads a number without a fraction that is exactly an integer, and
//! one past 2^53 isn't exact.

use super::{Object, Value};
use serde::de::{self, DeserializeOwned, IntoDeserializer, Visitor};
use serde::ser::{self, Serialize};
use serde::{Deserialize, Deserializer, Serializer, forward_to_deserialize_any};
use std::fmt;

/// Why a value couldn't be written or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerdeError(pub String);

impl fmt::Display for SerdeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SerdeError {}

impl ser::Error for SerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerdeError(msg.to_string())
    }
}

impl de::Error for SerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerdeError(msg.to_string())
    }
}

/// `value` as a JSON value.
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, SerdeError> {
    value.serialize(ValueSerializer)
}

/// `value` read as a `T`.
pub fn from_value<T: DeserializeOwned>(value: &Value) -> Result<T, SerdeError> {
    T::deserialize(value)
}

// MARK: Writing

struct ValueSerializer;

/// The largest integer a `f64` holds exactly, 2^53.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

fn variant(name: &str, value: Value) -> Value {
    let mut o = Object::new();
    o.insert(name, value);
    Value::Object(o)
}

impl Serializer for ValueSerializer {
    type Ok = Value;
    type Error = SerdeError;
    type SerializeSeq = SeqSerializer;
    type SerializeTuple = SeqSerializer;
    type SerializeTupleStruct = SeqSerializer;
    type SerializeTupleVariant = SeqSerializer;
    type SerializeMap = MapSerializer;
    type SerializeStruct = MapSerializer;
    type SerializeStructVariant = MapSerializer;

    fn serialize_bool(self, v: bool) -> Result<Value, SerdeError> {
        Ok(Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<Value, SerdeError> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_u8(self, v: u8) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Value, SerdeError> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_f32(self, v: f32) -> Result<Value, SerdeError> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, SerdeError> {
        Ok(Value::Number(v))
    }
    fn serialize_char(self, v: char) -> Result<Value, SerdeError> {
        Ok(Value::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Value, SerdeError> {
        Ok(Value::String(v.to_owned()))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Value, SerdeError> {
        Ok(Value::Array(
            v.iter().map(|b| Value::Number((*b).into())).collect(),
        ))
    }
    fn serialize_none(self) -> Result<Value, SerdeError> {
        Ok(Value::Null)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value, SerdeError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value, SerdeError> {
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, SerdeError> {
        Ok(Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Value, SerdeError> {
        Ok(Value::String(variant.to_owned()))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Value, SerdeError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        value: &T,
    ) -> Result<Value, SerdeError> {
        Ok(variant(name, value.serialize(self)?))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSerializer, SerdeError> {
        Ok(SeqSerializer {
            variant: None,
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqSerializer, SerdeError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<SeqSerializer, SerdeError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        len: usize,
    ) -> Result<SeqSerializer, SerdeError> {
        Ok(SeqSerializer {
            variant: Some(name),
            items: Vec::with_capacity(len),
        })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<MapSerializer, SerdeError> {
        Ok(MapSerializer {
            variant: None,
            object: Object::new(),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<MapSerializer, SerdeError> {
        self.serialize_map(None)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        _: usize,
    ) -> Result<MapSerializer, SerdeError> {
        Ok(MapSerializer {
            variant: Some(name),
            object: Object::new(),
            key: None,
        })
    }
}

struct SeqSerializer {
    variant: Option<&'static str>,
    items: Vec<Value>,
}

impl SeqSerializer {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        self.items.push(value.serialize(ValueSerializer)?);
        Ok(())
    }

    fn done(self) -> Value {
        let array = Value::Array(self.items);
        match self.variant {
            Some(name) => variant(name, array),
            None => array,
        }
    }
}

impl ser::SerializeSeq for SeqSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        self.push(value)
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl ser::SerializeTuple for SeqSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        self.push(value)
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl ser::SerializeTupleStruct for SeqSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        self.push(value)
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl ser::SerializeTupleVariant for SeqSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        self.push(value)
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

struct MapSerializer {
    variant: Option<&'static str>,
    object: Object,
    key: Option<String>,
}

impl MapSerializer {
    fn done(self) -> Value {
        let object = Value::Object(self.object);
        match self.variant {
            Some(name) => variant(name, object),
            None => object,
        }
    }
}

impl ser::SerializeMap for MapSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), SerdeError> {
        // A key is a string, as a JS object's property is: a number or a
        // bool key is written as JS would name it.
        let key = match key.serialize(ValueSerializer)? {
            Value::String(s) => s,
            v @ (Value::Number(_) | Value::Bool(_)) => v.to_string(),
            v => {
                return Err(SerdeError(format!(
                    "a map key must be a string, not {}",
                    v.type_name()
                )));
            }
        };
        self.key = Some(key);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerdeError> {
        let key = self.key.take().expect("serialize_key comes first");
        self.object.insert(key, value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl ser::SerializeStruct for MapSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SerdeError> {
        self.object.insert(key, value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl ser::SerializeStructVariant for MapSerializer {
    type Ok = Value;
    type Error = SerdeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SerdeError> {
        ser::SerializeStruct::serialize_field(self, key, value)
    }
    fn end(self) -> Result<Value, SerdeError> {
        Ok(self.done())
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            // An integer as an integer, so a format with integers keeps it
            // one.
            Value::Number(n) if n.fract() == 0.0 && n.abs() < MAX_EXACT => {
                s.serialize_i64(*n as i64)
            }
            Value::Number(n) => s.serialize_f64(*n),
            Value::String(v) => s.serialize_str(v),
            Value::Array(a) => s.collect_seq(a),
            Value::Object(o) => o.serialize(s),
        }
    }
}

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.iter())
    }
}

// MARK: Reading

impl<'de> Deserializer<'de> for &'de Value {
    type Error = SerdeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SerdeError> {
        match self {
            Value::Null => visitor.visit_unit(),
            Value::Bool(b) => visitor.visit_bool(*b),
            // An exact integer reads as one, so integer fields take it.
            Value::Number(n) if n.fract() == 0.0 && n.abs() < MAX_EXACT => {
                if *n < 0.0 {
                    visitor.visit_i64(*n as i64)
                } else {
                    visitor.visit_u64(*n as u64)
                }
            }
            Value::Number(n) => visitor.visit_f64(*n),
            Value::String(s) => visitor.visit_borrowed_str(s),
            Value::Array(a) => visitor.visit_seq(de::value::SeqDeserializer::new(a.iter())),
            Value::Object(o) => visitor.visit_map(de::value::MapDeserializer::new(
                o.iter().map(|(k, v)| (k.as_str(), v)),
            )),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SerdeError> {
        match self {
            Value::Null => visitor.visit_none(),
            v => visitor.visit_some(v),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, SerdeError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, SerdeError> {
        match self {
            // A unit variant, by name.
            Value::String(s) => visitor.visit_enum(s.as_str().into_deserializer()),
            // Any other variant, as `{"<name>": <its value>}`.
            Value::Object(o) if o.len() == 1 => {
                let (name, value) = o.get_index(0).expect("one entry");
                visitor.visit_enum(EnumAccess { name, value })
            }
            v => Err(SerdeError(format!(
                "expected an enum variant, a string or an object with one key, got {}",
                v.type_name()
            ))),
        }
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }
}

impl<'de> IntoDeserializer<'de, SerdeError> for &'de Value {
    type Deserializer = Self;
    fn into_deserializer(self) -> Self {
        self
    }
}

struct EnumAccess<'de> {
    name: &'de str,
    value: &'de Value,
}

impl<'de> de::EnumAccess<'de> for EnumAccess<'de> {
    type Error = SerdeError;
    type Variant = &'de Value;
    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, &'de Value), SerdeError> {
        let name = seed.deserialize(self.name.into_deserializer())?;
        Ok((name, self.value))
    }
}

impl<'de> de::VariantAccess<'de> for &'de Value {
    type Error = SerdeError;
    fn unit_variant(self) -> Result<(), SerdeError> {
        match self {
            Value::Null => Ok(()),
            v => Err(SerdeError(format!(
                "expected a unit variant, got {}",
                v.type_name()
            ))),
        }
    }
    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, SerdeError> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, SerdeError> {
        self.deserialize_seq(visitor)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, SerdeError> {
        self.deserialize_map(visitor)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        d.deserialize_any(ValueVisitor)
    }
}

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Object, D::Error> {
        match Value::deserialize(d)? {
            Value::Object(o) => Ok(o),
            v => Err(de::Error::custom(format!(
                "expected an object, got {}",
                v.type_name()
            ))),
        }
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v as f64))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v as f64))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Number(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        Value::deserialize(d)
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(v) = seq.next_element()? {
            items.push(v);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut o = Object::new();
        while let Some((k, v)) = map.next_entry::<String, Value>()? {
            o.insert(k, v);
        }
        Ok(Value::Object(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        project_name: String,
        #[serde(rename = "issueID")]
        issue_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<i64>,
        dir: Dir,
        ratio: f64,
        tags: Vec<String>,
        shape: Shape,
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Dir {
        Asc,
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Shape {
        Circle { r: u32 },
        Pair(i32, i32),
    }

    #[test]
    fn round_trips_in_declared_field_order() {
        let args = Args {
            project_name: "Zero".into(),
            issue_id: None,
            limit: Some(-3),
            dir: Dir::Asc,
            ratio: 0.5,
            tags: vec!["a".into()],
            shape: Shape::Circle { r: 2 },
        };
        let v = to_value(&args).unwrap();
        // Fields in the order they are declared, as `JSON.stringify` writes
        // an object literal with the same properties.
        assert_eq!(
            json::stringify(&v),
            r#"{"projectName":"Zero","issueID":null,"limit":-3,"dir":"asc","ratio":0.5,"tags":["a"],"shape":{"Circle":{"r":2}}}"#
        );
        assert_eq!(from_value::<Args>(&v).unwrap(), args);
        let pair = to_value(&Shape::Pair(1, -2)).unwrap();
        assert_eq!(json::stringify(&pair), r#"{"Pair":[1,-2]}"#);
        assert_eq!(from_value::<Shape>(&pair).unwrap(), Shape::Pair(1, -2));
    }

    #[test]
    fn integers_read_only_when_exact() {
        assert_eq!(from_value::<i64>(&Value::Number(4.0)).unwrap(), 4);
        assert_eq!(from_value::<u8>(&Value::Number(255.0)).unwrap(), 255);
        assert!(from_value::<i64>(&Value::Number(4.5)).is_err());
        assert!(from_value::<u8>(&Value::Number(-1.0)).is_err());
        assert_eq!(from_value::<f64>(&Value::Number(4.0)).unwrap(), 4.0);
        // A missing optional field and a null one both read as `None`.
        let v = json::parse(
            r#"{"projectName":"Zero","dir":"asc","ratio":1,"tags":[],"shape":{"Pair":[0,0]}}"#,
        )
        .unwrap();
        let args: Args = from_value(&v).unwrap();
        assert_eq!((args.issue_id, args.limit, args.ratio), (None, None, 1.0));
        assert!(from_value::<Args>(&json::parse(r#"{"projectName":1}"#).unwrap()).is_err());
    }

    #[test]
    fn values_are_themselves() {
        let text = r#"{"b":[1,2.5,null,true,"x"],"10":{},"a":-7}"#;
        let v = json::parse(text).unwrap();
        let again: Value = from_value(&to_value(&v).unwrap()).unwrap();
        assert_eq!(json::stringify(&again), json::stringify(&v));
    }
}
