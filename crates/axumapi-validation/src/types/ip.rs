//! IP address newtypes wrapping [`std::net`] types.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;
use serde_json::Value;

/// An IPv4 or IPv6 address.
///
/// Pydantic `IPvAnyAddress` also accepts integers; this type accepts only
/// address strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IpAddress(IpAddr);

impl IpAddress {
    /// Parse `value` as an IPv4 or IPv6 address.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        rules::parse_ip(value.as_ref()).map(Self)
    }
}

crate::types::impl_wrapper!(IpAddress => IpAddr);

impl TryFrom<String> for IpAddress {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for IpAddress {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<IpAddress> for String {
    fn from(value: IpAddress) -> Self {
        value.0.to_string()
    }
}

impl Validate for IpAddress {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for IpAddress {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::default().with(
            "anyOf",
            Value::Array(vec![
                SchemaObject::of_type("string")
                    .with("format", "ipv4")
                    .into_value(),
                SchemaObject::of_type("string")
                    .with("format", "ipv6")
                    .into_value(),
            ]),
        )
    }
}

impl Dump for IpAddress {}

/// An IPv4 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ipv4Address(Ipv4Addr);

impl Ipv4Address {
    /// Parse `value` as an IPv4 address.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        rules::parse_ipv4(value.as_ref()).map(Self)
    }
}

crate::types::impl_wrapper!(Ipv4Address => Ipv4Addr);

impl TryFrom<String> for Ipv4Address {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Ipv4Address {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ipv4Address> for String {
    fn from(value: Ipv4Address) -> Self {
        value.0.to_string()
    }
}

impl Validate for Ipv4Address {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for Ipv4Address {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "ipv4")
    }
}

impl Dump for Ipv4Address {}

/// An IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ipv6Address(Ipv6Addr);

impl Ipv6Address {
    /// Parse `value` as an IPv6 address.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        rules::parse_ipv6(value.as_ref()).map(Self)
    }
}

crate::types::impl_wrapper!(Ipv6Address => Ipv6Addr);

impl TryFrom<String> for Ipv6Address {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Ipv6Address {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ipv6Address> for String {
    fn from(value: Ipv6Address) -> Self {
        value.0.to_string()
    }
}

impl Validate for Ipv6Address {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for Ipv6Address {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "ipv6")
    }
}

impl Dump for Ipv6Address {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    #[test]
    fn ip_construction() {
        assert_eq!(
            IpAddress::new("127.0.0.1").unwrap().into_inner(),
            IpAddr::from([127, 0, 0, 1])
        );
        assert!(IpAddress::new("::1").is_ok());
        assert_eq!(IpAddress::new("nope").unwrap_err().code, "ip_any_parsing");
        assert_eq!(Ipv4Address::new("::1").unwrap_err().code, "ip_v4_parsing");
        assert_eq!(
            Ipv6Address::new("127.0.0.1").unwrap_err().code,
            "ip_v6_parsing"
        );
        assert!(Ipv4Address::new("192.168.0.1").is_ok());
        assert!(Ipv6Address::new("2001:db8::1").is_ok());
    }

    #[test]
    fn ip_serde_round_trip_and_reject() {
        let ip = Ipv4Address::new("10.0.0.1").unwrap();
        let encoded = serde_json::to_string(&ip).unwrap();
        assert_eq!(encoded, "\"10.0.0.1\"");
        let round: Ipv4Address = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round.into_inner(), Ipv4Addr::new(10, 0, 0, 1));
        assert!(serde_json::from_str::<Ipv4Address>("\"::1\"").is_err());
        assert!(serde_json::from_str::<IpAddress>("\"not-an-ip\"").is_err());
    }

    #[test]
    fn ip_prepare_lax_and_strict() {
        let (out, codes) = prepare_codes::<IpAddress>(json!("127.0.0.1"), ValidationContext::new());
        assert!(codes.is_empty());
        assert_eq!(out, json!("127.0.0.1"));
        assert_eq!(
            prepare_codes::<IpAddress>(json!(1), ValidationContext::new()).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<IpAddress>(json!(1), ValidationContext::new().with_strict(true)).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<Ipv6Address>(json!("127.0.0.1"), ValidationContext::new()).1,
            ["ip_v6_parsing"]
        );
    }

    #[test]
    fn ip_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<Ipv4Address>().0.into_value(),
            json!({"type": "string", "format": "ipv4"})
        );
        assert_eq!(
            schema_for::<Ipv6Address>().0.into_value(),
            json!({"type": "string", "format": "ipv6"})
        );
        assert_eq!(
            schema_for::<IpAddress>().0.into_value(),
            json!({
                "anyOf": [
                    {"type": "string", "format": "ipv4"},
                    {"type": "string", "format": "ipv6"}
                ]
            })
        );
        let ip = IpAddress::new("::1").unwrap();
        assert_eq!(ip.dump(&DumpOptions::new()).unwrap(), json!("::1"));
        let parsed =
            crate::parse_value::<Ipv4Address>(json!("1.2.3.4"), ValidationContext::new()).unwrap();
        assert_eq!(parsed.to_string(), "1.2.3.4");
        let err =
            crate::parse_value::<IpAddress>(json!("nope"), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "ip_any_parsing");
    }
}
