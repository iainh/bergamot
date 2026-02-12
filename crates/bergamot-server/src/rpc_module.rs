use std::sync::Arc;

use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;

use crate::error::JsonRpcError;
use crate::rpc::dispatch_rpc;
use crate::server::AppState;

fn to_error_object(err: JsonRpcError) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(err.code, err.message, None::<()>)
}

pub fn build_rpc_module(state: Arc<AppState>) -> RpcModule<Arc<AppState>> {
    let mut module = RpcModule::new(state);

    fn reg(module: &mut RpcModule<Arc<AppState>>, name: &'static str) {
        module
            .register_async_method(name, move |params, ctx, _| async move {
                let v: serde_json::Value = params
                    .parse()
                    .unwrap_or(serde_json::json!([]));
                dispatch_rpc(name, &v, &ctx)
                    .await
                    .map_err(to_error_object)
            })
            .expect("register method");
    }

    for name in [
        "version",
        "status",
        "append",
        "listgroups",
        "editqueue",
        "shutdown",
        "listfiles",
        "postqueue",
        "writelog",
        "loadlog",
        "log",
        "servervolumes",
        "schedulerstats",
        "resetservervolume",
        "config",
        "loadconfig",
        "saveconfig",
        "configtemplates",
        "history",
        "rate",
        "pausedownload",
        "resumedownload",
        "pausepost",
        "resumepost",
        "pausescan",
        "resumescan",
        "scan",
        "feeds",
        "sysinfo",
        "systemhealth",
        "loadextensions",
        "testserver",
        "editserver",
        "scheduleresume",
        "reload",
        "clearlog",
        "readurl",
        "testextension",
    ] {
        reg(&mut module, name);
    }

    module
}
