use std::borrow::Cow;

use crate::{
    ContextSelectionSet, OutputType, Positioned, ServerResult, Value,
    extensions::{ResolveFut, ResolveInfo},
    parser::types::Field,
};

type BoxValueFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = ServerResult<Value>> + Send + 'a>>;

/// Non-generic join, so `try_join_all` is compiled once instead of once per item type.
#[inline(never)]
async fn join_list(futures: Vec<BoxValueFuture<'_>>) -> ServerResult<Value> {
    Ok(Value::List(
        futures_util::future::try_join_all(futures).await?,
    ))
}

/// Resolve an list by executing each of the items concurrently.
pub async fn resolve_list<'a, T: OutputType + 'a>(
    ctx: &ContextSelectionSet<'a>,
    field: &Positioned<Field>,
    iter: impl IntoIterator<Item = T>,
    len: Option<usize>,
) -> ServerResult<Value> {
    let hooks = !ctx.query_env.extensions.is_empty();
    let mut futures: Vec<BoxValueFuture<'_>> = len.map(Vec::with_capacity).unwrap_or_default();
    for (idx, item) in iter.into_iter().enumerate() {
        let ctx_idx = ctx.with_index(idx);
        futures.push(Box::pin(async move {
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
        }));
    }
    join_list(futures).await
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
