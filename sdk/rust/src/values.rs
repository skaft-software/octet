//! Typed JSON serialization must not silently turn NaN/infinity into null.
use serde::ser::*;
use serde::Serialize;
use serde_json::Value;

pub(crate) const MAX_INTEGER: i64 = 9_007_199_254_740_991;

pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Value, crate::Error> {
    serde_json::to_value(Checked(value)).map_err(|_| crate::Error::internal())
}
struct Checked<'a, T: ?Sized>(&'a T);
impl<T: Serialize + ?Sized> Serialize for Checked<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(Strict(serializer))
    }
}
struct Strict<S>(S);
macro_rules! scalar {
    ($($name:ident($ty:ty)),* $(,)?) => {$(
        fn $name(self, value: $ty) -> Result<Self::Ok, Self::Error> { self.0.$name(value) }
    )*};
}
macro_rules! integer {
    ($($name:ident($ty:ty)),* $(,)?) => {$(
        fn $name(self, value: $ty) -> Result<Self::Ok, Self::Error> {
            if !(-(MAX_INTEGER as i128)..=MAX_INTEGER as i128).contains(&(value as i128)) {
                return Err(S::Error::custom("nonportable JSON integer"));
            }
            self.0.$name(value)
        }
    )*};
}
impl<S: Serializer> Serializer for Strict<S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = Strict<S::SerializeSeq>;
    type SerializeTuple = Strict<S::SerializeTuple>;
    type SerializeTupleStruct = Strict<S::SerializeTupleStruct>;
    type SerializeTupleVariant = Strict<S::SerializeTupleVariant>;
    type SerializeMap = Strict<S::SerializeMap>;
    type SerializeStruct = Strict<S::SerializeStruct>;
    type SerializeStructVariant = Strict<S::SerializeStructVariant>;
    scalar!(
        serialize_bool(bool),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8])
    );
    integer!(
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64)
    );
    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        if value > MAX_INTEGER as u128 {
            return Err(S::Error::custom("nonportable JSON integer"));
        }
        self.0.serialize_u128(value)
    }
    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        if !value.is_finite() {
            return Err(S::Error::custom("nonfinite scalar"));
        }
        self.0.serialize_f32(value)
    }
    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        if !value.is_finite() {
            return Err(S::Error::custom("nonfinite scalar"));
        }
        self.0.serialize_f64(value)
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_none()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_some(&Checked(v))
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit()
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit_struct(name)
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit_variant(name, index, variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_newtype_struct(name, &Checked(v))
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0
            .serialize_newtype_variant(name, index, variant, &Checked(v))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.0.serialize_seq(len).map(Strict)
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.0.serialize_tuple(len).map(Strict)
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.0.serialize_tuple_struct(name, len).map(Strict)
    }
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.0
            .serialize_tuple_variant(name, index, variant, len)
            .map(Strict)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        self.0.serialize_map(len).map(Strict)
    }
    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.0.serialize_struct(name, len).map(Strict)
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.0
            .serialize_struct_variant(name, index, variant, len)
            .map(Strict)
    }
    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}
macro_rules! sequence {
    ($trait:ident, $method:ident) => {
        impl<S: $trait> $trait for Strict<S> {
            type Ok = S::Ok;
            type Error = S::Error;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
                self.0.$method(&Checked(value))
            }
            fn end(self) -> Result<Self::Ok, Self::Error> {
                self.0.end()
            }
        }
    };
}
sequence!(SerializeSeq, serialize_element);
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);
sequence!(SerializeTupleVariant, serialize_field);
macro_rules! record {
    ($trait:ident) => {
        impl<S: $trait> $trait for Strict<S> {
            type Ok = S::Ok;
            type Error = S::Error;
            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), Self::Error> {
                self.0.serialize_field(key, &Checked(value))
            }
            fn end(self) -> Result<Self::Ok, Self::Error> {
                self.0.end()
            }
        }
    };
}
record!(SerializeStruct);
record!(SerializeStructVariant);
impl<S: SerializeMap> SerializeMap for Strict<S> {
    type Ok = S::Ok;
    type Error = S::Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        self.0.serialize_key(&Checked(key))
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.0.serialize_value(&Checked(value))
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_nonfinite_even_inside_nullable_containers() {
        for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(encode(&vec![Some(n)]).is_err());
        }
        assert!(encode(&Some(MAX_INTEGER + 1)).is_err());
        assert!(encode(&Some(u128::MAX)).is_err());
        assert_eq!(
            encode(&vec![Some(1.25), None]).unwrap(),
            serde_json::json!([1.25, null])
        );
    }
}
