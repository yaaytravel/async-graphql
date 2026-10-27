use std::future::Future;

use futures_util::future::Either;
use std::{
    borrow::Cow,
    sync::{Arc, Weak},
};

use async_graphql_value::ConstValue;

use crate::{
    ContainerType, Context, ContextSelectionSet, Error, InputValueError, InputValueResult,
    Positioned, Result, ServerResult, Value,
    parser::types::Field,
    registry::{self, Registry},
};

#[doc(hidden)]
pub trait Description {
    fn description() -> &'static str;
}

/// Used to specify the GraphQL Type name.
pub trait TypeName: Send + Sync {
    /// Returns a GraphQL type name.
    fn type_name() -> Cow<'static, str>;
}

/// Represents a GraphQL input type.
pub trait InputType: Send + Sync + Sized {
    /// The raw type used for validator.
    ///
    /// Usually it is `Self`, but the wrapper type is its internal type.
    ///
    /// For example:
    ///
    /// `i32::RawValueType` is `i32`
    /// `Option<i32>::RawValueType` is `i32`.
    type RawValueType: ?Sized;

    /// Type the name.
    fn type_name() -> Cow<'static, str>;

    /// Qualified typename.
    fn qualified_type_name() -> String {
        format!("{}!", Self::type_name())
    }

    /// Create type information in the registry and return qualified typename.
    fn create_type_info(registry: &mut registry::Registry) -> String;

    /// Parse from `Value`. None represents undefined.
    fn parse(value: Option<Value>) -> InputValueResult<Self>;

    /// Convert to a `Value` for introspection.
    fn to_value(&self) -> Value;

    /// Get the federation fields, only for InputObject.
    #[doc(hidden)]
    fn federation_fields() -> Option<String> {
        None
    }

    /// Returns a reference to the raw value.
    fn as_raw_value(&self) -> Option<&Self::RawValueType>;
}

/// Represents a GraphQL output type.
pub trait OutputType: Send + Sync {
    /// Type the name.
    fn type_name() -> Cow<'static, str>;

    /// Qualified typename.
    fn qualified_type_name() -> String {
        format!("{}!", Self::type_name())
    }

    /// Introspection type name
    ///
    /// Is the return value of field `__typename`, the interface and union
    /// should return the current type, and the others return `Type::type_name`.
    fn introspection_type_name(&self) -> Cow<'static, str> {
        Self::type_name()
    }

    /// Create type information in the registry and return qualified typename.
    fn create_type_info(registry: &mut registry::Registry) -> String;

    /// Returns the value directly when resolving it needs neither the selection
    /// set nor any async work, as for scalars and enums. Lists use this to skip
    /// creating a future for each item.
    #[doc(hidden)]
    fn resolve_now(&self) -> Option<Value> {
        None
    }

    /// Resolve an output value to `async_graphql::Value`.
    ///
    /// The value, the context and the field share one lifetime, so an
    /// implementation can return another value's future directly instead of
    /// wrapping it in a state machine of its own. An `async fn` with separate
    /// lifetimes still implements it.
    fn resolve<'a>(
        &'a self,
        ctx: &'a ContextSelectionSet<'a>,
        field: &'a Positioned<Field>,
    ) -> impl Future<Output = ServerResult<Value>> + Send + 'a;
}

impl<T: OutputType + ?Sized> OutputType for &T {
    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn resolve_now(&self) -> Option<Value> {
        T::resolve_now(*self)
    }

    // Returns the inner future rather than wrapping it in another state machine.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn resolve<'a>(
        &'a self,
        ctx: &'a ContextSelectionSet<'a>,
        field: &'a Positioned<Field>,
    ) -> impl Future<Output = ServerResult<Value>> + Send + 'a {
        T::resolve(*self, ctx, field)
    }
}

impl<T: OutputType + Sync, E: Into<Error> + Send + Sync + Clone> OutputType for Result<T, E> {
    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn resolve_now(&self) -> Option<Value> {
        // An error goes through `resolve`, which sets its path and position.
        self.as_ref().ok().and_then(T::resolve_now)
    }

    fn resolve<'a>(
        &'a self,
        ctx: &'a ContextSelectionSet<'a>,
        field: &'a Positioned<Field>,
    ) -> impl Future<Output = ServerResult<Value>> + Send + 'a {
        match self {
            Ok(value) => Either::Left(value.resolve(ctx, field)),
            Err(err) => Either::Right(std::future::ready(Err(
                ctx.set_error_path(err.clone().into().into_server_error(field.pos))
            ))),
        }
    }
}

/// A GraphQL object.
pub trait ObjectType: ContainerType {}

impl<T: ObjectType + ?Sized> ObjectType for &T {}

impl<T: ObjectType + ?Sized> ObjectType for Box<T> {}

impl<T: ObjectType + ?Sized> ObjectType for Arc<T> {}

/// A GraphQL interface.
pub trait InterfaceType: ContainerType {}

/// A GraphQL interface.
pub trait UnionType: ContainerType {}

/// A GraphQL input object.
pub trait InputObjectType: InputType {}

/// A GraphQL oneof input object.
pub trait OneofObjectType: InputObjectType {}

impl<T: OutputType + ?Sized> OutputType for Box<T> {
    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn resolve_now(&self) -> Option<Value> {
        T::resolve_now(self.as_ref())
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn resolve<'a>(
        &'a self,
        ctx: &'a ContextSelectionSet<'a>,
        field: &'a Positioned<Field>,
    ) -> impl Future<Output = ServerResult<Value>> + Send + 'a {
        T::resolve(self.as_ref(), ctx, field)
    }
}

impl<T: InputType> InputType for Box<T> {
    type RawValueType = T::RawValueType;

    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn parse(value: Option<ConstValue>) -> InputValueResult<Self> {
        T::parse(value)
            .map(Box::new)
            .map_err(InputValueError::propagate)
    }

    fn to_value(&self) -> ConstValue {
        T::to_value(&self)
    }

    fn as_raw_value(&self) -> Option<&Self::RawValueType> {
        self.as_ref().as_raw_value()
    }
}

impl<T: OutputType + ?Sized> OutputType for Arc<T> {
    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn resolve_now(&self) -> Option<Value> {
        T::resolve_now(&**self)
    }

    // Returns the inner future rather than wrapping it in another state machine.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn resolve<'a>(
        &'a self,
        ctx: &'a ContextSelectionSet<'a>,
        field: &'a Positioned<Field>,
    ) -> impl Future<Output = ServerResult<Value>> + Send + 'a {
        T::resolve(&**self, ctx, field)
    }
}

impl<T: InputType> InputType for Arc<T> {
    type RawValueType = T::RawValueType;

    fn type_name() -> Cow<'static, str> {
        T::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        T::create_type_info(registry)
    }

    fn parse(value: Option<ConstValue>) -> InputValueResult<Self> {
        T::parse(value)
            .map(Arc::new)
            .map_err(InputValueError::propagate)
    }

    fn to_value(&self) -> ConstValue {
        T::to_value(&self)
    }

    fn as_raw_value(&self) -> Option<&Self::RawValueType> {
        self.as_ref().as_raw_value()
    }
}

impl<T: OutputType + ?Sized> OutputType for Weak<T> {
    fn type_name() -> Cow<'static, str> {
        <Option<Arc<T>> as OutputType>::type_name()
    }

    fn create_type_info(registry: &mut Registry) -> String {
        <Option<Arc<T>> as OutputType>::create_type_info(registry)
    }

    async fn resolve(
        &self,
        ctx: &ContextSelectionSet<'_>,
        field: &Positioned<Field>,
    ) -> ServerResult<Value> {
        self.upgrade().resolve(ctx, field).await
    }
}

#[doc(hidden)]
pub trait ComplexObject {
    fn fields(registry: &mut registry::Registry) -> Vec<(String, registry::MetaField)>;

    fn resolve_field(
        &self,
        ctx: &Context<'_>,
    ) -> impl Future<Output = ServerResult<Option<Value>>> + Send;
}
