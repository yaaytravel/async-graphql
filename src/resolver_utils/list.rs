use std::{borrow::Cow, future::Future};

use futures_util::TryFutureExt;

use crate::{
    ContextSelectionSet, OutputType, Positioned, ServerResult, Value,
    extensions::{ResolveFut, ResolveInfo},
    parser::types::Field,
};

type BoxValueFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = ServerResult<Value>> + Send + 'a>>;

/// Waits for the items that needed a future and puts each result in its place.
/// Not generic, so `try_join_all` is compiled once instead of once per item type.
#[inline(never)]
async fn join_list(
    mut values: Vec<Value>,
    pending: Vec<(usize, BoxValueFuture<'_>)>,
) -> ServerResult<Value> {
    if !pending.is_empty() {
        let futures = pending
            .into_iter()
            .map(|(idx, fut)| fut.map_ok(move |value| (idx, value)));
        for (idx, value) in futures_util::future::try_join_all(futures).await? {
            values[idx] = value;
        }
    }
    Ok(Value::List(values))
}

/// Resolve an list by executing each of the items concurrently.
///
/// Items that resolve without the selection set or async work (scalars and
/// enums) are stored directly, without a future, unless extensions need to see
/// each item resolve.
///
/// A plain function rather than an `async fn`: it starts every item before
/// returning, and the future it returns is the same type for every `T`, so no
/// state machine is generated per item type.
pub fn resolve_list<'b, T: OutputType + 'b>(
    ctx: &'b ContextSelectionSet<'_>,
    field: &'b Positioned<Field>,
    iter: impl IntoIterator<Item = T>,
    len: Option<usize>,
) -> impl Future<Output = ServerResult<Value>> + Send + 'b {
    let hooks = !ctx.query_env.extensions.is_empty();
    let mut values: Vec<Value> = len.map(Vec::with_capacity).unwrap_or_default();
    let mut pending: Vec<(usize, BoxValueFuture<'_>)> = Vec::new();
    for (idx, item) in iter.into_iter().enumerate() {
        if !hooks && let Some(value) = item.resolve_now() {
            values.push(value);
            continue;
        }
        values.push(Value::Null);
        if pending.capacity() == 0 {
            // Items of one type usually all need a future, or none do.
            pending.reserve(len.unwrap_or_default());
        }
        let ctx_idx = ctx.with_index(idx);
        pending.push((
            idx,
            Box::pin(async move {
                if hooks {
                    let resolve_fut = async {
                        OutputType::resolve(&item, &ctx_idx, field)
                            .await
                            .map(Option::Some)
                            .map_err(|err| ctx_idx.set_error_path(err))
                    };
                    futures_util::pin_mut!(resolve_fut);
                    resolve_item_with_hooks(
                        &ctx_idx,
                        field,
                        Vec::<T>::type_name(),
                        T::qualified_type_name(),
                        &mut resolve_fut,
                    )
                    .await
                } else {
                    OutputType::resolve(&item, &ctx_idx, field)
                        .await
                        .map_err(|err| ctx_idx.set_error_path(err))
                }
            }),
        ));
    }
    join_list(values, pending)
}

/// Runs a list item's resolver through the request's extensions. Not generic,
/// so it is compiled once rather than once per item type.
async fn resolve_item_with_hooks(
    ctx_idx: &ContextSelectionSet<'_>,
    field: &Positioned<Field>,
    parent_type: Cow<'static, str>,
    return_type: String,
    resolve_fut: ResolveFut<'_>,
) -> ServerResult<Value> {
    let resolve_info = ResolveInfo {
        path_node: ctx_idx.path_node.as_ref().unwrap(),
        parent_type: &parent_type,
        return_type: &return_type,
        name: field.node.name.node.as_str(),
        alias: field.node.alias.as_ref().map(|alias| alias.node.as_str()),
        is_for_introspection: ctx_idx.is_for_introspection,
        field: &field.node,
    };
    ctx_idx
        .query_env
        .extensions
        .resolve(resolve_info, resolve_fut)
        .await
        .map(|value| value.expect("You definitely encountered a bug!"))
}
