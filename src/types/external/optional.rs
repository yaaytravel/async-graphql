use std::borrow::Cow;

use crate::{
    ContextSelectionSet, InputType, InputValueError, InputValueResult, OutputType, Positioned,
    ServerResult, Value, parser::types::Field, registry,
};

impl<T: InputType> InputType for Option<T> {
    type RawValueType = T::RawValueType;

    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn qualified_type_name() -> String {
        T::type_name().to_string()
    }

    fn create_type_info(registry: &mut registry::Registry) -> String {
        T::create_type_info(registry);
        T::type_name().to_string()
    }

    fn parse(value: Option<Value>) -> InputValueResult<Self> {
        match value.unwrap_or_default() {
            Value::Null => Ok(None),
            value => Ok(Some(
                T::parse(Some(value)).map_err(InputValueError::propagate)?,
            )),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Some(value) => value.to_value(),
            None => Value::Null,
        }
    }

    fn as_raw_value(&self) -> Option<&Self::RawValueType> {
        match self {
            Some(value) => value.as_raw_value(),
            None => None,
        }
    }
}

impl<T: OutputType + Sync> OutputType for Option<T> {
    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn qualified_type_name() -> String {
        T::type_name().to_string()
    }

    fn create_type_info(registry: &mut registry::Registry) -> String {
        T::create_type_info(registry);
        T::type_name().to_string()
    }

    fn resolve_now(&self) -> Option<Value> {
        match self {
            Some(inner) => inner.resolve_now(),
            None => Some(Value::Null),
        }
    }

    async fn resolve(
        &self,
        ctx: &ContextSelectionSet<'_>,
        field: &Positioned<Field>,
    ) -> ServerResult<Value> {
        match self {
            Some(inner) => Ok(null_on_error(
                ctx,
                OutputType::resolve(inner, ctx, field).await,
            )),
            None => Ok(Value::Null),
        }
    }
}

/// A nullable field's value: an error is recorded on the request and becomes
/// `null`. Not generic, so it is compiled once rather than once per type.
#[inline(never)]
fn null_on_error(ctx: &ContextSelectionSet<'_>, result: ServerResult<Value>) -> Value {
    result.unwrap_or_else(|err| {
        ctx.add_error(err);
        Value::Null
    })
}

#[cfg(test)]
mod tests {
    use crate::InputType;

    #[test]
    fn test_optional_type() {
        assert_eq!(Option::<i32>::type_name(), "Int");
        assert_eq!(Option::<i32>::qualified_type_name(), "Int");
        assert_eq!(&Option::<i32>::type_name(), "Int");
        assert_eq!(&Option::<i32>::qualified_type_name(), "Int");
    }
}
