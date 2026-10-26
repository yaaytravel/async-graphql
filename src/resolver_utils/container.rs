use std::{borrow::Cow, future::Future, pin::Pin, sync::Arc, task::Poll};

use indexmap::IndexMap;

use crate::{
    Context, ContextBase, ContextSelectionSet, Error, Name, OutputType, QueryPathNode,
    QueryPathSegment, ServerError, ServerResult, Value,
    extensions::{ResolveFut, ResolveInfo},
    parser::types::Selection,
};

/// Represents a GraphQL container object.
///
/// This helper trait allows the type to call `resolve_container` on itself in
/// its `OutputType::resolve` implementation.
#[cfg_attr(feature = "boxed-trait", async_trait::async_trait)]
pub trait ContainerType: OutputType {
    /// This function returns true of type `EmptyMutation` only.
    #[doc(hidden)]
    fn is_empty() -> bool {
        false
    }

    /// Resolves a field value and outputs it as a json value
    /// `async_graphql::Value`.
    ///
    /// If the field was not found returns None.
    #[cfg(feature = "boxed-trait")]
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>>;

    /// Resolves a field value and outputs it as a json value
    /// `async_graphql::Value`.
    ///
    /// If the field was not found returns None.
    #[cfg(not(feature = "boxed-trait"))]
    fn resolve_field(
        &self,
        ctx: &Context<'_>,
    ) -> impl Future<Output = ServerResult<Option<Value>>> + Send;

    /// Collect all the fields of the container that are queried in the
    /// selection set.
    ///
    /// Objects do not have to override this, but interfaces and unions must
    /// call it on their internal type.
    fn collect_all_fields<'a>(
        &'a self,
        ctx: &ContextSelectionSet<'a>,
        fields: &mut Fields<'a>,
    ) -> ServerResult<()>
    where
        Self: Send + Sync,
    {
        fields.add_set(ctx, self)
    }

    /// Find the GraphQL entity with the given name from the parameter.
    ///
    /// Objects should override this in case they are the query root.
    #[cfg(feature = "boxed-trait")]
    async fn find_entity(&self, _: &Context<'_>, _: &Value) -> ServerResult<Option<Value>> {
        Ok(None)
    }

    /// Find the GraphQL entity with the given name from the parameter.
    ///
    /// Objects should override this in case they are the query root.
    #[cfg(not(feature = "boxed-trait"))]
    fn find_entity(
        &self,
        _: &Context<'_>,
        _params: &Value,
    ) -> impl Future<Output = ServerResult<Option<Value>>> + Send {
        async { Ok(None) }
    }
}

#[cfg_attr(feature = "boxed-trait", async_trait::async_trait)]
impl<T: ContainerType + ?Sized> ContainerType for &T {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(*self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(*self, ctx, params).await
    }
}

#[cfg_attr(feature = "boxed-trait", async_trait::async_trait)]
impl<T: ContainerType + ?Sized> ContainerType for Arc<T> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(self, ctx, params).await
    }
}

#[cfg_attr(feature = "boxed-trait", async_trait::async_trait)]
impl<T: ContainerType + ?Sized> ContainerType for Box<T> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        T::resolve_field(self, ctx).await
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        T::find_entity(self, ctx, params).await
    }
}

#[cfg_attr(feature = "boxed-trait", async_trait::async_trait)]
impl<T: ContainerType, E: Into<Error> + Send + Sync + Clone> ContainerType for Result<T, E> {
    async fn resolve_field(&self, ctx: &Context<'_>) -> ServerResult<Option<Value>> {
        match self {
            Ok(value) => T::resolve_field(value, ctx).await,
            Err(err) => Err(ctx.set_error_path(err.clone().into().into_server_error(ctx.item.pos))),
        }
    }

    async fn find_entity(&self, ctx: &Context<'_>, params: &Value) -> ServerResult<Option<Value>> {
        match self {
            Ok(value) => T::find_entity(value, ctx, params).await,
            Err(err) => Err(ctx.set_error_path(err.clone().into().into_server_error(ctx.item.pos))),
        }
    }
}

#[doc(hidden)]
pub type BoxFieldResult<'a> =
    Pin<Box<dyn Future<Output = ServerResult<Option<Value>>> + Send + 'a>>;

/// Object-safe view of a borrowed [`ContainerType`], so field collection is
/// compiled once instead of once per GraphQL type.
///
/// The derive macros implement it for `&Self` on every type they generate, so
/// resolving one object type and then the next goes through no generic library
/// code. Generic code instantiated once per type would otherwise nest once per
/// type along a chain of object types and hit rustc's recursion limit.
/// Hand-written containers go through [`GenericContainer`] instead.
#[doc(hidden)]
pub trait ErasedContainer<'a>: Send + Sync {
    /// Resolves a field. With `hooks`, the field also runs through the
    /// request's extensions and the field's custom directives.
    fn erased_resolve_field(&self, ctx: Context<'a>, hooks: bool) -> BoxFieldResult<'a>;
    /// The container's GraphQL type name.
    fn erased_type_name(&self) -> Cow<'static, str>;
    /// The container's concrete type name for `__typename` and fragments.
    fn erased_introspection_type_name(&self) -> Cow<'static, str>;
    /// Collects the container's fields selected by `ctx`.
    fn erased_collect_all_fields(
        &self,
        ctx: &ContextSelectionSet<'a>,
        fields: &mut Fields<'a>,
    ) -> ServerResult<()>;
}

/// The generic [`ErasedContainer`] for containers the derive macros did not
/// generate, such as hand-written `ContainerType` impls.
#[doc(hidden)]
pub struct GenericContainer<'a, T: ?Sized>(pub &'a T);

impl<'a, T: ContainerType + ?Sized> ErasedContainer<'a> for GenericContainer<'a, T> {
    fn erased_resolve_field(&self, ctx: Context<'a>, hooks: bool) -> BoxFieldResult<'a> {
        let root = self.0;
        if hooks {
            Box::pin(async move {
                let resolve_fut = root.resolve_field(&ctx);
                futures_util::pin_mut!(resolve_fut);
                resolve_field_with_hooks(&ctx, T::type_name(), &mut resolve_fut).await
            })
        } else {
            Box::pin(async move { root.resolve_field(&ctx).await })
        }
    }

    fn erased_type_name(&self) -> Cow<'static, str> {
        T::type_name()
    }

    fn erased_introspection_type_name(&self) -> Cow<'static, str> {
        T::introspection_type_name(self.0)
    }

    fn erased_collect_all_fields(
        &self,
        ctx: &ContextSelectionSet<'a>,
        fields: &mut Fields<'a>,
    ) -> ServerResult<()> {
        T::collect_all_fields(self.0, ctx, fields)
    }
}

/// Resolve an container by executing each of the fields concurrently.
pub fn resolve_container<'a, T: ContainerType + ?Sized>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a T,
) -> impl Future<Output = ServerResult<Value>> + Send {
    join_fields(Fields::collect(ctx, root), true)
}

/// Resolve an container by executing each of the fields serially.
pub fn resolve_container_serial<'a, T: ContainerType + ?Sized>(
    ctx: &ContextSelectionSet<'a>,
    root: &'a T,
) -> impl Future<Output = ServerResult<Value>> + Send {
    join_fields(Fields::collect(ctx, root), false)
}

/// Resolve an erased container, concurrently or serially. The derive macros
/// call this with their generated [`ErasedContainer`] impl.
#[doc(hidden)]
pub fn resolve_container_dyn<'a>(
    ctx: &'a ContextSelectionSet<'a>,
    root: &dyn ErasedContainer<'a>,
    parallel: bool,
) -> impl Future<Output = ServerResult<Value>> + Send + 'a {
    let mut fields = Fields::new(ctx);
    let fields = fields.add_set_erased(ctx, root).map(|()| fields);
    join_fields(fields, parallel)
}

pub(crate) fn create_value_object(values: Vec<(Name, Value)>) -> Value {
    let mut map = IndexMap::with_capacity(values.len());
    for (name, value) in values {
        insert_value(&mut map, name, value);
    }
    Value::Object(map)
}

fn insert_value(target: &mut IndexMap<Name, Value>, name: Name, value: Value) {
    if let Some(prev_value) = target.get_mut(&name) {
        if let Value::Object(target_map) = prev_value {
            if let Value::Object(obj) = value {
                for (key, value) in obj.into_iter() {
                    insert_value(target_map, key, value);
                }
            }
        } else if let Value::List(target_list) = prev_value
            && let Value::List(list) = value
        {
            for (idx, value) in list.into_iter().enumerate() {
                if let Some(Value::Object(target_map)) = target_list.get_mut(idx)
                    && let Value::Object(obj) = value
                {
                    for (key, value) in obj.into_iter() {
                        insert_value(target_map, key, value);
                    }
                }
            }
        }
    } else {
        target.insert(name, value);
    }
}

/// Above this many fields, `try_join_all` (which only polls the fields that
/// were woken) is used instead of polling every pending field on each wake.
const JOIN_IN_PLACE_MAX: usize = 32;

async fn join_fields(fields: ServerResult<Fields<'_>>, parallel: bool) -> ServerResult<Value> {
    let fields = fields?.futures;
    if parallel && fields.len() > JOIN_IN_PLACE_MAX {
        let res = futures_util::future::try_join_all(fields.into_iter().map(|field| async move {
            let value = match field.state {
                FieldState::Pending(fut) => fut.await?.unwrap_or_default(),
                FieldState::Done(value) => value,
            };
            Ok::<_, ServerError>((field.name, value))
        }))
        .await?;
        return Ok(create_value_object(res));
    }
    JoinFields { fields, parallel }.await
}

/// Waits for every field of a container in place: each slot's future is
/// replaced by its value, and the values are moved into the result object, so
/// no other list is allocated. Serially, a field is only polled once every
/// field before it has its value.
struct JoinFields<'a> {
    fields: Vec<FieldFuture<'a>>,
    parallel: bool,
}

impl Future for JoinFields<'_> {
    type Output = ServerResult<Value>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut done = true;
        for field in &mut this.fields {
            if let FieldState::Pending(fut) = &mut field.state {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(value)) => {
                        field.state = FieldState::Done(value.unwrap_or_default())
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending if this.parallel => done = false,
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
        if !done {
            return Poll::Pending;
        }
        let mut map = IndexMap::with_capacity(this.fields.len());
        for field in this.fields.drain(..) {
            if let FieldState::Done(value) = field.state {
                insert_value(&mut map, field.name, value);
            }
        }
        Poll::Ready(Ok(Value::Object(map)))
    }
}

/// Runs a field's resolver through the request's extensions and the field's
/// custom directives.
#[doc(hidden)]
pub async fn resolve_field_with_hooks(
    ctx_field: &Context<'_>,
    type_name: Cow<'static, str>,
    resolve_fut: ResolveFut<'_>,
) -> ServerResult<Option<Value>> {
    let field = ctx_field.item;
    let field_name = &field.node.response_key().node;
    let extensions = &ctx_field.query_env.extensions;
    let resolve_info = ResolveInfo {
        path_node: ctx_field.path_node.as_ref().unwrap(),
        parent_type: &type_name,
        return_type: match ctx_field
            .schema_env
            .registry
            .types
            .get(type_name.as_ref())
            .and_then(|ty| ty.field_by_name(field.node.name.node.as_str()))
            .map(|field| &field.ty)
        {
            Some(ty) => &ty,
            None => {
                return Err(ServerError::new(
                    format!(
                        r#"Cannot query field "{}" on type "{}"."#,
                        field_name, type_name
                    ),
                    Some(ctx_field.item.pos),
                ));
            }
        },
        name: field.node.name.node.as_str(),
        alias: field.node.alias.as_ref().map(|alias| alias.node.as_str()),
        is_for_introspection: ctx_field.is_for_introspection,
        field: &field.node,
    };

    if field.node.directives.is_empty() {
        return extensions.resolve(resolve_info, resolve_fut).await;
    }

    let mut resolve_fut: BoxFieldResult<'_> = Box::pin(resolve_fut);
    for directive in &field.node.directives {
        if let Some(directive_factory) = ctx_field
            .schema_env
            .custom_directives
            .get(directive.node.name.node.as_str())
        {
            let ctx_directive = ContextBase {
                path_node: ctx_field.path_node,
                is_for_introspection: false,
                item: directive,
                schema_env: ctx_field.schema_env,
                query_env: ctx_field.query_env,
                execute_data: ctx_field.execute_data,
            };
            let directive_instance = directive_factory.create(&ctx_directive, &directive.node)?;
            resolve_fut = Box::pin({
                let ctx_field = ctx_field.clone();
                async move {
                    directive_instance
                        .resolve_field(&ctx_field, &mut resolve_fut)
                        .await
                }
            });
        }
    }

    extensions.resolve(resolve_info, &mut resolve_fut).await
}

/// A selected field: its response key, and the future that resolves its value
/// until the value is there.
struct FieldFuture<'a> {
    name: Name,
    state: FieldState<'a>,
}

enum FieldState<'a> {
    Pending(BoxFieldResult<'a>),
    Done(Value),
}

/// A set of fields on an container that are being selected.
pub struct Fields<'a> {
    futures: Vec<FieldFuture<'a>>,
    /// Path of the container. Every field context points at this node rather
    /// than at the `ContextSelectionSet` it was collected from, because
    /// fragment selection sets are collected through temporary contexts that
    /// carry a copy of the same path.
    parent_path: Option<&'a QueryPathNode<'a>>,
}

impl<'a> Fields<'a> {
    fn new(ctx: &'a ContextSelectionSet<'a>) -> Self {
        Fields {
            futures: Vec::with_capacity(ctx.item.node.items.len()),
            parent_path: ctx.path_node.as_ref(),
        }
    }

    fn collect<T: ContainerType + ?Sized>(
        ctx: &'a ContextSelectionSet<'a>,
        root: &'a T,
    ) -> ServerResult<Self> {
        let mut fields = Fields::new(ctx);
        fields.add_set(ctx, root)?;
        Ok(fields)
    }

    fn push(&mut self, name: Name, value: BoxFieldResult<'a>) {
        self.futures.push(FieldFuture {
            name,
            state: FieldState::Pending(value),
        });
    }

    /// Add another set of fields to this set of fields using the given
    /// container.
    pub fn add_set<T: ContainerType + ?Sized>(
        &mut self,
        ctx: &ContextSelectionSet<'a>,
        root: &'a T,
    ) -> ServerResult<()> {
        self.add_set_erased(ctx, &GenericContainer(root))
    }

    /// Add another set of fields using an erased container. The derive macros
    /// call this from their generated `collect_all_fields`.
    #[doc(hidden)]
    pub fn add_set_erased(
        &mut self,
        ctx: &ContextSelectionSet<'a>,
        root: &dyn ErasedContainer<'a>,
    ) -> ServerResult<()> {
        for selection in &ctx.item.node.items {
            match &selection.node {
                Selection::Field(field) => {
                    let ctx_field = ContextBase {
                        path_node: Some(QueryPathNode {
                            parent: self.parent_path,
                            segment: QueryPathSegment::Name(&field.node.response_key().node),
                        }),
                        is_for_introspection: ctx.is_for_introspection,
                        item: field,
                        schema_env: ctx.schema_env,
                        query_env: ctx.query_env,
                        execute_data: ctx.execute_data,
                    };
                    let field_name = ctx_field.item.node.response_key().node.clone();

                    if field.node.name.node == "__typename" {
                        let typename = root.erased_introspection_type_name().into_owned();
                        self.futures.push(FieldFuture {
                            name: field_name,
                            state: FieldState::Done(Value::String(typename)),
                        });
                        continue;
                    }

                    let hooks = ctx.query_env.extensions.has_resolve_hooks()
                        || !field.node.directives.is_empty();
                    self.push(field_name, root.erased_resolve_field(ctx_field, hooks));
                }
                selection => {
                    let (type_condition, selection_set) = match selection {
                        Selection::Field(_) => unreachable!(),
                        Selection::FragmentSpread(spread) => {
                            let fragment =
                                ctx.query_env.fragments.get(&spread.node.fragment_name.node);
                            let fragment = match fragment {
                                Some(fragment) => fragment,
                                None => {
                                    return Err(ServerError::new(
                                        format!(
                                            r#"Unknown fragment "{}"."#,
                                            spread.node.fragment_name.node
                                        ),
                                        Some(spread.pos),
                                    ));
                                }
                            };
                            (
                                Some(&fragment.node.type_condition),
                                &fragment.node.selection_set,
                            )
                        }
                        Selection::InlineFragment(fragment) => (
                            fragment.node.type_condition.as_ref(),
                            &fragment.node.selection_set,
                        ),
                    };
                    let type_condition =
                        type_condition.map(|condition| condition.node.on.node.as_str());

                    let introspection_type_name = root.erased_introspection_type_name();

                    let applies_concrete_object = type_condition.is_some_and(|condition| {
                        introspection_type_name == condition
                            || ctx
                                .schema_env
                                .registry
                                .implements
                                .get(&*introspection_type_name)
                                .is_some_and(|interfaces| interfaces.contains(condition))
                    });
                    if applies_concrete_object {
                        root.erased_collect_all_fields(
                            &ctx.with_selection_set(selection_set),
                            self,
                        )?;
                    } else if type_condition
                        .is_none_or(|condition| root.erased_type_name() == condition)
                    {
                        // The fragment applies to an interface type.
                        self.add_set_erased(&ctx.with_selection_set(selection_set), root)?;
                    }
                }
            }
        }
        Ok(())
    }
}
