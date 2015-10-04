//Local files/dependencies
mod config;

//External dependencies
#[macro_use]
extern crate clap;
extern crate hyper;
extern crate jsonrpc;
#[macro_use]
extern crate log;
extern crate rustc_serialize;
extern crate yaml_rust;

use clap::App;
use config::{ServerConfig, MethodDefinition, Variable};
use hyper::status::StatusCode;
use hyper::server::{Server, Request, Response, Handler};
use hyper::uri::{RequestUri};
use hyper::net::Openssl;
use jsonrpc::{JsonRpcServer, JsonRpcRequest, ErrorCode, ErrorCodeData};
use log::{LogRecord, LogLevel, LogMetadata};
use rustc_serialize::json::{ToJson, Json};
use std::thread;
use std::io::{Read, BufReader, BufRead, Write};
use std::process::{Command, Stdio};
use std::collections::{BTreeMap, HashMap};
use yaml_rust::YamlLoader;


struct SimpleLogger;
struct SenderHandler {
    //unique client request tracing?
    request_id: u32,
    json_rpc: JsonRpcServer<RpcHandler>,
    config: ServerConfig
}
struct RpcHandler {
    methods: HashMap<String, MethodDefinition>
}

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &LogMetadata) -> bool {
        metadata.level() <= LogLevel::Info
    }

    fn log(&self, record: &LogRecord) {
        if self.enabled(record.metadata()) {
            println!("{} - {}", record.level(), record.args());
        }
    }
}

impl Handler for SenderHandler {
    fn handle(&self, req: Request, mut res: Response) {
        //Only support of POST
        info!("Processing request from {}. Method {}. Uri {}", req.remote_addr, req.method, req.uri);
        if req.method == hyper::Post {
            if let RequestUri::AbsolutePath(ref path) = req.uri.clone() {
                match path as &str {
                "/streaming" => self.handle_streaming(req, res),
                "/jsonrpc" => self.handle_json_rpc(req, res),
                _ => {
                    error!("Unknown request path: {}", path);
                    *res.status_mut() = StatusCode::NotFound
                    }
                }
            }
        } else {
            *res.status_mut() = StatusCode::MethodNotAllowed;
        }
    }
}


impl RpcHandler {
    pub fn new(methods: HashMap<String, MethodDefinition>) -> RpcHandler {
        RpcHandler {
            methods: methods
        }
    }
}

impl SenderHandler {
    fn new(conf: ServerConfig) -> SenderHandler {
        //Dont like it...
        let json_handler = RpcHandler::new(conf.methods.clone());

        SenderHandler {
            request_id: 0,
            json_rpc: JsonRpcServer::new_handler(json_handler),
            config: conf
        }
    }

    fn handle_streaming(&self, mut req: Request, mut res: Response) {
        //Read streaming method name from path
        // POST /streaming
        // {"method": "enter method name here", "params": "Optional params", "id": "optional id"}
        let mut request_str = String::new();
        //TODO: Limit read size
        req.read_to_string(&mut request_str).unwrap();
        info!("--> {}", request_str);
        let request_docs = YamlLoader::load_from_str(&request_str).unwrap();
        let request_params = &request_docs[0];

        let method_name = if let Some(s) = request_params["method"].as_str() {
            s
        } else {
            *res.status_mut() = StatusCode::NotFound;
            return;
        };
        
        let method = self.config.streams.get(method_name);
        if method.is_none() {
            warn!("Requested method {} not found", method_name);
            *res.status_mut() = StatusCode::NotFound;
            return;
        }
        let method = method.unwrap();

        //For now: No parameters parsing
        let mut arguments = Vec::new();
        for arg in &method.exec_params {
            arguments.push(arg);
        }
        info!("Spawn child for {} with args: {:?}", method_name, arguments);

        //Spawn child object
        let child_process = Command::new(&method.path)
            .args(&arguments)
            .stdout(Stdio::piped())
            .spawn().unwrap();

        //Pipe stdout
        let stdout_stream = child_process.stdout.unwrap();
        let mut streaming_response = res.start().unwrap();
        //Read as hytes chunks
        let reader = BufReader::new(stdout_stream);
        for line in reader.split(b'\n') {
            let line = line.unwrap();
            //Ignore all non utf8 characters (well this is log anyway)
            info!("<-- {}", String::from_utf8_lossy(&line));
            //Respond to client with content "as-is" 
            streaming_response.write(&line).unwrap();
            streaming_response.write(b"\n").unwrap();
            streaming_response.flush().unwrap();
        }
        info!("Reading STDOUT finished");
        streaming_response.end().unwrap();
        //todo: Detect when connection is killed and kill child process
    }

    fn handle_json_rpc(&self, mut req: Request, res: Response) {
        //TODO: check required content type
        let mut request = String::new();
        if req.read_to_string(&mut request).is_err() {
            warn!("Unable to read request");
            res.send(b"Bah!").unwrap();
            return;
        }
        info!("Request: {}", request);
        let response = self.json_rpc.handle_request(&request);
        if let Some(response) = response {
            info!("Response: {}", response);
            res.send(&response.into_bytes()).unwrap();
        } else {
            info!("Just notification");
        }
    }
}

impl jsonrpc::Handler for RpcHandler {
    fn handle(&self, req: &JsonRpcRequest) -> Result<Json, ErrorCodeData> {
        info!("Call from callback!");
        let method = self.methods.get(&req.method);
        if method.is_none() {
            error!("Requested method '{}' not found!", &req.method);
            return Err(ErrorCodeData::Without(ErrorCode::MethodNotFound))
        }
        let method = method.unwrap();
       
        //TODO: For now hackish solution
        let params = if let Some(ref p) = req.params {
            p.as_object().unwrap().to_owned()
        } else {
            //Report error only if we need some parameters
            if !method.variables.is_empty() {
                return Err(ErrorCodeData::Without(ErrorCode::InvalidParams));
            }
            BTreeMap::new()
        };

        //prepare arguments
        let mut arguments = Vec::new();
        for arg in &method.exec_params {
            let mut arg = arg.clone();
            info!("Argument before evaluation {}", arg);
            for (key,value) in &method.variables {
                info!("Evaluation: {}", key);
                match *value {
                    Variable::Named(ref name) => {
                        if let Some(value) = params.get(name) {
                        arg = arg.replace(key, value.as_string().unwrap());
                        } else {
                            error!("Requested parameters {} not found!", key);
                            return Err(ErrorCodeData::Without(ErrorCode::InvalidParams));
                        }
                    }
                    _ => {}
                };
                //arg = arg.replace(var.key, req.params[var.value]
            }
            info!("Argument after evaluation {}", arg);
            arguments.push(arg);
        }
        if let Some(ref fake_response) = method.use_fake_response {
            //delayed response...
            info!("Delayed command execution. Faking response {}", fake_response);
            let path = method.path.clone();
            thread::spawn(move || {
                thread::sleep_ms(2000);
                info!("Executing delayed command");
                match Command::new(&path).args(&arguments).output() {
                    Ok(o) => {
                        info!("Execution finished\nStatus: {}\nStdout: {}\nStderr: {}\n",
                               o.status,
                               String::from_utf8_lossy(&o.stdout),
                               String::from_utf8_lossy(&o.stderr));
                    },
                    Err(e) => info!("Failed to execute process: {}", e)
                }
                
            });
            return Ok(fake_response.clone());
        } else {
            let output = Command::new(&method.path)
                .args(&arguments)
                .output()
                .map(|o|String::from_utf8_lossy(&o.stdout).to_json())
                .map_err(|_| ErrorCodeData::Without(ErrorCode::InvalidParams));
            return output;
        }
    }
}

fn set_log_level(level: log::LogLevelFilter) {
    if let Err(e) = log::set_logger(|max_log_level| {
        max_log_level.set(level);
        Box::new(SimpleLogger)
    }) {
        println!("Log framework failed {}", e);
    }

}

/**
 * Main entry point
 * */
fn main() {
    //set default sane log level
    set_log_level(log::LogLevelFilter::Info);

    let yml = load_yaml!("app.yml");
    let m = App::from_yaml(yml).get_matches();

    let config_file = m.value_of("config").unwrap();
    let config = ServerConfig::read_from_file(config_file);
    set_log_level(config.log_level);

    if config.use_https {
        let ssl = Openssl::with_cert_and_key(&config.cert.as_ref().unwrap(), &config.key.as_ref().unwrap()).unwrap();
        Server::https((&config.address as &str, config.port), ssl).unwrap().handle(SenderHandler::new(config)).unwrap();
    } else {
        Server::http((&config.address as &str, config.port)).unwrap().handle(SenderHandler::new(config)).unwrap();
    }
}
