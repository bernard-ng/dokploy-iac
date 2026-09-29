use std::{borrow::Cow, fmt};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer};

/// Whether a configuration field is unmanaged, explicitly cleared, or set.
#[derive(Clone, Default, Eq, PartialEq)]
pub enum Field<T> {
    /// The key was omitted, so Dokploy's current value is not managed.
    #[default]
    Unmanaged,
    /// The key was present as YAML `null`, so the remote value is cleared.
    Clear,
    /// The key was present with a concrete managed value.
    Set(T),
}

impl<T> fmt::Debug for Field<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmanaged => formatter.write_str("Unmanaged"),
            Self::Clear => formatter.write_str("Clear"),
            Self::Set(_) => formatter.write_str("Set([REDACTED])"),
        }
    }
}

impl<T> Field<T> {
    /// Returns the managed value when the field is set.
    #[must_use]
    pub const fn as_set(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Unmanaged | Self::Clear => None,
        }
    }
}

impl<'de, T> Deserialize<'de> for Field<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| value.map_or(Self::Clear, Self::Set))
    }
}

impl<T> JsonSchema for Field<T>
where
    T: JsonSchema,
{
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Field_of_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("dokploy_config::Field<{}>", T::schema_id()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Option<T>>::json_schema(generator)
    }

    fn _schemars_private_non_optional_json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::_schemars_private_non_optional_json_schema(generator)
    }

    fn _schemars_private_is_option() -> bool {
        true
    }
}
