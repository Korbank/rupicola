
use config::*;
use jsonrpc::{JsonRpcServer, JsonRpcRequest, ErrorCode, ErrorJsonRpc, Handler};
use rustc_serialize::json::{ToJson, Json};
use rustc_serialize::base64::{STANDARD, ToBase64};
use std::thread;
use std::time::Duration;
use std::process::Command;
use std::collections::HashMap;
use params::{Unroll, MethodParam};
use handlers::{ResponseHandler, HandlerError};
use std::io::{Read, Write};

pub struct RpcHandler {
    methods: HashMap<String, MethodDefinition>,
}

impl RpcHandler {
    pub fn new(methods: HashMap<String, MethodDefinition>) -> RpcHandler {
        RpcHandler { methods: methods }
    }
}

impl Handler for RpcHandler {
    fn handle(&self, req: &JsonRpcRequest, custom: &HashMap<&str, Json>) -> Result<Json, ErrorJsonRpc> {
        let method = if let Some(s) = self.methods.get(req.method) {
            s
        } else {
            error!("Requested method '{}' not found!", req.method);
            return Err(ErrorJsonRpc::new(ErrorCode::MethodNotFound));
        };
        // TODO: Check if (!private && !auth)
        // Anyway this is unable to send back info about unauthorized request
        let is_auth = custom["is_auth"].as_boolean().unwrap_or(false);
        if !is_auth && !method.is_private {
            error!("You shall not pass");
            return Err(ErrorJsonRpc::new(ErrorCode::ServerError(-32000, "Unauthorized")));
        }
        // TODO: For now hackish solution
        // Allow not only objects but also arrays
        let params = if let Some(p) = req.params {
            p.to_owned()
        } else {
            Json::Null
        };
        // prepare arguments
        let arguments = if let Ok(ok) = get_invoke_arguments(&method.exec_params, &params) {
            ok
        } else {
            error!("Invalid params for request");
            return Err(ErrorJsonRpc::new(ErrorCode::InvalidParams));
        };

        info!("Method invoke with {:?}", arguments);

        if let Some(ref fake_response) = method.use_fake_response {
            // delayed response...
            info!("Delayed command execution. Faking response {}",
                  fake_response);
            let path = method.path.clone();
            let delay = method.delay as u64;
            thread::spawn(move || {
                thread::sleep(Duration::new(delay, 0));
                info!("Executing delayed ({}ms) command", delay);
                match Command::new(&path).args(&arguments).output() {
                    Ok(o) => {
                        // Log as lossy utf8.
                        // TODO: Limit output size? Eg cat on whole partition?
                        info!("Execution finished\nStatus: {}\nStdout: {}\nStderr: {}\n",
                              o.status,
                              String::from_utf8_lossy(&o.stdout),
                              String::from_utf8_lossy(&o.stderr));
                    }
                    Err(e) => info!("Failed to execute process: {}", e),
                }
            });
            //This method support only utf-8 (we just spit whole json from config...)
            return Ok(fake_response.clone());
        } else {
            //Encode to baseXY?
            let output = Command::new(&method.path)
                    .args(&arguments)
                    .output()
                    .map(|o| {
                        if method.response_encoding == ResponseEncoding::Utf8 {
                            String::from_utf8_lossy(&o.stdout).to_json()
                        } else {
                            o.stdout.to_base64(STANDARD).to_json()
                        }
                    })
                    .map_err(|_| ErrorJsonRpc::new(ErrorCode::ServerError(-32001, "Subprocedure failed to run")));
            return output;
        }
    }
}

pub fn get_invoke_arguments(exec_params: &Vec<MethodParam>, params: &Json) -> Result<Vec<String>, ()> {
    let mut arguments = Vec::new();
    for arg in exec_params {
        match arg.unroll(&params) {
            Ok(Some(s)) => arguments.push(s),
            Err(_) => return Err(()),
            // We dont care about Ok(None)
            _ => {}
        }
    }
    Ok(arguments)
}

impl ResponseHandler for JsonRpcServer<RpcHandler> {
    fn handle_response(&self, req: &mut Read, res: &mut Write, is_auth: bool) -> Result<(), HandlerError> {
        // TODO: check required content type
        let mut request = String::new();
        if req.read_to_string(&mut request).is_err() {
            warn!("Unable to read request");
            return Err(HandlerError::InvalidRequest);
        }
        info!("Request: {}", request);
        let mut custom_data = HashMap::new();
        custom_data.insert("is_auth", is_auth.to_json());
        let response = self.handle_request_custom(&request, Some(&custom_data));
        if let Some(response) = response {
            info!("Response: {}", response);
            if let Err(e) = res.write(&response.into_bytes()) {
                error!("Error during sending response: {:?}", e);
                return Err(HandlerError::InvalidRequest);
            }
        }
        Ok(())
    }
}
