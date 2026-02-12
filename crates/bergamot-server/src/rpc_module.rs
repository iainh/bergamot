use std::sync::Arc;

use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;

use crate::error::JsonRpcError;
use crate::rpc;
use crate::server::AppState;

fn to_error_object(err: JsonRpcError) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(err.code, err.message, None::<()>)
}

fn parse_params(
    params: jsonrpsee::types::Params<'_>,
) -> Result<serde_json::Value, ErrorObjectOwned> {
    let v: Option<serde_json::Value> = params
        .parse()
        .map_err(|e| ErrorObjectOwned::owned(-32602, e.to_string(), None::<()>))?;
    Ok(v.unwrap_or_else(|| serde_json::json!([])))
}

pub fn build_rpc_module(state: Arc<AppState>) -> RpcModule<Arc<AppState>> {
    let mut module = RpcModule::new(state);

    module
        .register_async_method("version", |_, ctx, _| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!(ctx.version()))
        })
        .expect("register method");

    module
        .register_async_method("status", |_, ctx, _| async move {
            serde_json::to_value(ctx.status())
                .map_err(|e| ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>))
        })
        .expect("register method");

    module
        .register_async_method("append", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_append(&v, &ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("listgroups", |_, ctx, _| async move {
            rpc::rpc_listgroups(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("editqueue", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_editqueue(&v, &ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("shutdown", |_, ctx, _| async move {
            rpc::rpc_shutdown(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("listfiles", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_listfiles(&v, &ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("postqueue", |_, ctx, _| async move {
            rpc::rpc_postqueue(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("writelog", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_writelog(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("loadlog", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_loadlog(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("log", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_loadlog(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("servervolumes", |_, ctx, _| async move {
            rpc::rpc_servervolumes(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("schedulerstats", |_, ctx, _| async move {
            rpc::rpc_schedulerstats(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("resetservervolume", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_resetservervolume(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("config", |_, ctx, _| async move {
            rpc::rpc_loadconfig(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("loadconfig", |_, ctx, _| async move {
            rpc::rpc_loadconfig(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("saveconfig", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_saveconfig(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("configtemplates", |_, _, _| async move {
            rpc::rpc_configtemplates().map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("history", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_history(&v, &ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("rate", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_rate(&v, &ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("pausedownload", |_, ctx, _| async move {
            rpc::rpc_pausedownload(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("resumedownload", |_, ctx, _| async move {
            rpc::rpc_resumedownload(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("pausepost", |_, ctx, _| async move {
            rpc::rpc_pausepost(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("resumepost", |_, ctx, _| async move {
            rpc::rpc_resumepost(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("pausescan", |_, ctx, _| async move {
            rpc::rpc_pausescan(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("resumescan", |_, ctx, _| async move {
            rpc::rpc_resumescan(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("scan", |_, ctx, _| async move {
            rpc::rpc_scan(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("feeds", |_, ctx, _| async move {
            rpc::rpc_feeds(&ctx).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("sysinfo", |_, ctx, _| async move {
            rpc::rpc_sysinfo(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("systemhealth", |_, ctx, _| async move {
            rpc::rpc_systemhealth(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("loadextensions", |_, _, _| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!([]))
        })
        .expect("register method");

    module
        .register_async_method("testserver", |params, _, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_testserver(&v).await.map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("editserver", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_editserver(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("scheduleresume", |params, ctx, _| async move {
            let v = parse_params(params)?;
            rpc::rpc_scheduleresume(&v, &ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("reload", |_, ctx, _| async move {
            rpc::rpc_reload(&ctx).map_err(to_error_object)
        })
        .expect("register method");

    module
        .register_async_method("clearlog", |_, ctx, _| async move {
            if let Some(buffer) = ctx.log_buffer() {
                buffer.clear();
            }
            Ok::<_, ErrorObjectOwned>(serde_json::json!(true))
        })
        .expect("register method");

    module
        .register_async_method("readurl", |params, _, _| async move {
            let v = parse_params(params)?;
            let arr = v.as_array();
            let url = arr
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match reqwest::get(url).await {
                Ok(resp) => {
                    let body = resp.text().await.unwrap_or_default();
                    Ok(serde_json::json!(body))
                }
                Err(e) => Err(to_error_object(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                })),
            }
        })
        .expect("register method");

    module
        .register_async_method("testextension", |_, _, _| async move {
            Ok::<_, ErrorObjectOwned>(serde_json::json!(""))
        })
        .expect("register method");

    module
}
